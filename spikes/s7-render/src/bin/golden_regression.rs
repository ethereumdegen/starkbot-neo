use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use image::{DynamicImage, Rgba, RgbaImage};
use s7_render::fixtures::engine_drift_fixtures;
use serde::Serialize;

const DIRECT_TOLERANCE: u8 = 18;
const ANTIALIAS_TOLERANCE: u8 = 24;
const MAX_SIGNIFICANT_RATIO: f64 = 0.008;
const MIN_SSIM: f64 = 0.970;
const REGION_CELL: u32 = 24;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    passed: bool,
    reference_dir: String,
    actual_dir: String,
    thresholds: Thresholds,
    fixtures: Vec<FixtureReport>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Thresholds {
    direct_channel_delta: u8,
    antialias_channel_delta: u8,
    max_significant_ratio: f64,
    min_ssim: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FixtureReport {
    id: String,
    passed: bool,
    width: u32,
    height: u32,
    raw_different_ratio: f64,
    significant_ratio: f64,
    ssim: f64,
    regions: Vec<DiffRegion>,
    diff_path: String,
}

#[derive(Clone, Serialize)]
struct DiffRegion {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    pixels: u64,
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let reference_dir = args
        .next()
        .unwrap_or_else(|| PathBuf::from("spikes/fixtures/golden/chrome"));
    let actual_dir = args
        .next()
        .unwrap_or_else(|| PathBuf::from("spikes/out/golden/webkit"));
    let diff_dir = args
        .next()
        .unwrap_or_else(|| PathBuf::from("spikes/out/golden/diff"));
    if args.next().is_some() {
        bail!("usage: golden_regression [reference-dir] [actual-dir] [diff-dir]");
    }
    std::fs::create_dir_all(&diff_dir)?;

    let mut reports = Vec::new();
    for fixture in engine_drift_fixtures() {
        reports.push(compare_fixture(
            fixture.id,
            &reference_dir,
            &actual_dir,
            &diff_dir,
        )?);
    }
    let passed = reports.iter().all(|report| report.passed);
    let report = Report {
        passed,
        reference_dir: reference_dir.display().to_string(),
        actual_dir: actual_dir.display().to_string(),
        thresholds: Thresholds {
            direct_channel_delta: DIRECT_TOLERANCE,
            antialias_channel_delta: ANTIALIAS_TOLERANCE,
            max_significant_ratio: MAX_SIGNIFICANT_RATIO,
            min_ssim: MIN_SSIM,
        },
        fixtures: reports,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !passed {
        std::process::exit(1);
    }
    Ok(())
}

fn compare_fixture(
    id: &str,
    reference_dir: &Path,
    actual_dir: &Path,
    diff_dir: &Path,
) -> anyhow::Result<FixtureReport> {
    let reference_path = reference_dir.join(format!("{id}.png"));
    let actual_path = actual_dir.join(format!("{id}.png"));
    let reference = image::open(&reference_path)
        .with_context(|| format!("could not open reference {}", reference_path.display()))?
        .to_rgba8();
    let actual = image::open(&actual_path)
        .with_context(|| format!("could not open actual {}", actual_path.display()))?
        .to_rgba8();
    if reference.dimensions() != actual.dimensions() {
        bail!(
            "{id}: dimensions differ: reference {:?}, actual {:?}",
            reference.dimensions(),
            actual.dimensions()
        );
    }

    let (width, height) = reference.dimensions();
    let pixel_count = u64::from(width) * u64::from(height);
    let mut raw_different = 0_u64;
    let mut significant_count = 0_u64;
    let mut significant = vec![false; pixel_count as usize];
    let mut overlay = RgbaImage::new(width, height);

    for y in 0..height {
        for x in 0..width {
            let expected = *reference.get_pixel(x, y);
            let observed = *actual.get_pixel(x, y);
            let raw = channel_delta(expected, observed) > DIRECT_TOLERANCE;
            let meaningful = raw
                && !near_match(observed, &reference, x, y, ANTIALIAS_TOLERANCE)
                && !near_match(expected, &actual, x, y, ANTIALIAS_TOLERANCE);
            raw_different += u64::from(raw);
            significant_count += u64::from(meaningful);
            significant[(u64::from(y) * u64::from(width) + u64::from(x)) as usize] = meaningful;

            let base = Rgba([
                (u16::from(expected[0]) * 2 / 5) as u8,
                (u16::from(expected[1]) * 2 / 5) as u8,
                (u16::from(expected[2]) * 2 / 5) as u8,
                255,
            ]);
            overlay.put_pixel(
                x,
                y,
                if meaningful {
                    Rgba([255, 38, 76, 255])
                } else if raw {
                    Rgba([255, 190, 48, 230])
                } else {
                    base
                },
            );
        }
    }

    let regions = diff_regions(&significant, width, height);
    draw_regions(&mut overlay, &regions);
    let diff_path = diff_dir.join(format!("{id}.png"));
    DynamicImage::ImageRgba8(overlay)
        .save_with_format(&diff_path, image::ImageFormat::Png)
        .with_context(|| format!("could not write {}", diff_path.display()))?;

    let raw_different_ratio = raw_different as f64 / pixel_count as f64;
    let significant_ratio = significant_count as f64 / pixel_count as f64;
    let ssim = structural_similarity(&reference, &actual);
    let passed = significant_ratio <= MAX_SIGNIFICANT_RATIO && ssim >= MIN_SSIM;
    Ok(FixtureReport {
        id: id.to_owned(),
        passed,
        width,
        height,
        raw_different_ratio,
        significant_ratio,
        ssim,
        regions,
        diff_path: diff_path.display().to_string(),
    })
}

fn channel_delta(left: Rgba<u8>, right: Rgba<u8>) -> u8 {
    left.0
        .into_iter()
        .zip(right.0)
        .map(|(a, b)| a.abs_diff(b))
        .max()
        .unwrap_or_default()
}

fn near_match(pixel: Rgba<u8>, image: &RgbaImage, x: u32, y: u32, tolerance: u8) -> bool {
    let x0 = x.saturating_sub(1);
    let y0 = y.saturating_sub(1);
    let x1 = (x + 1).min(image.width() - 1);
    let y1 = (y + 1).min(image.height() - 1);
    (y0..=y1).any(|candidate_y| {
        (x0..=x1).any(|candidate_x| {
            channel_delta(pixel, *image.get_pixel(candidate_x, candidate_y)) <= tolerance
        })
    })
}

fn structural_similarity(reference: &RgbaImage, actual: &RgbaImage) -> f64 {
    const BLOCK: u32 = 8;
    const C1: f64 = 6.5025;
    const C2: f64 = 58.5225;
    let (width, height) = reference.dimensions();
    let mut score = 0.0;
    let mut blocks = 0_u64;
    for top in (0..height).step_by(BLOCK as usize) {
        for left in (0..width).step_by(BLOCK as usize) {
            let right = (left + BLOCK).min(width);
            let bottom = (top + BLOCK).min(height);
            let count = f64::from((right - left) * (bottom - top));
            let mut expected_sum = 0.0;
            let mut observed_sum = 0.0;
            for y in top..bottom {
                for x in left..right {
                    expected_sum += luminance(*reference.get_pixel(x, y));
                    observed_sum += luminance(*actual.get_pixel(x, y));
                }
            }
            let expected_mean = expected_sum / count;
            let observed_mean = observed_sum / count;
            let mut expected_variance = 0.0;
            let mut observed_variance = 0.0;
            let mut covariance = 0.0;
            for y in top..bottom {
                for x in left..right {
                    let expected = luminance(*reference.get_pixel(x, y)) - expected_mean;
                    let observed = luminance(*actual.get_pixel(x, y)) - observed_mean;
                    expected_variance += expected * expected;
                    observed_variance += observed * observed;
                    covariance += expected * observed;
                }
            }
            expected_variance /= count;
            observed_variance /= count;
            covariance /= count;
            let numerator = (2.0 * expected_mean * observed_mean + C1) * (2.0 * covariance + C2);
            let denominator = (expected_mean.powi(2) + observed_mean.powi(2) + C1)
                * (expected_variance + observed_variance + C2);
            score += if denominator == 0.0 {
                1.0
            } else {
                numerator / denominator
            };
            blocks += 1;
        }
    }
    score / blocks as f64
}

fn luminance(pixel: Rgba<u8>) -> f64 {
    0.2126 * f64::from(pixel[0]) + 0.7152 * f64::from(pixel[1]) + 0.0722 * f64::from(pixel[2])
}

fn diff_regions(mask: &[bool], width: u32, height: u32) -> Vec<DiffRegion> {
    let columns = width.div_ceil(REGION_CELL);
    let rows = height.div_ceil(REGION_CELL);
    let mut hot = vec![false; (columns * rows) as usize];
    let mut counts = vec![0_u64; hot.len()];
    for y in 0..height {
        for x in 0..width {
            if mask[(y * width + x) as usize] {
                let cell = (y / REGION_CELL) * columns + x / REGION_CELL;
                counts[cell as usize] += 1;
            }
        }
    }
    for row in 0..rows {
        for column in 0..columns {
            let cell = row * columns + column;
            let cell_width = REGION_CELL.min(width - column * REGION_CELL);
            let cell_height = REGION_CELL.min(height - row * REGION_CELL);
            let minimum = u64::from(cell_width * cell_height).div_ceil(200).max(4);
            hot[cell as usize] = counts[cell as usize] >= minimum;
        }
    }

    let mut seen = vec![false; hot.len()];
    let mut regions = Vec::new();
    for start in 0..hot.len() {
        if !hot[start] || seen[start] {
            continue;
        }
        let mut queue = VecDeque::from([start as u32]);
        seen[start] = true;
        let mut min_column = columns;
        let mut min_row = rows;
        let mut max_column = 0;
        let mut max_row = 0;
        let mut pixels = 0_u64;
        while let Some(cell) = queue.pop_front() {
            let row = cell / columns;
            let column = cell % columns;
            min_column = min_column.min(column);
            min_row = min_row.min(row);
            max_column = max_column.max(column);
            max_row = max_row.max(row);
            pixels += counts[cell as usize];
            for row_delta in -1_i32..=1 {
                for column_delta in -1_i32..=1 {
                    let next_row = row as i32 + row_delta;
                    let next_column = column as i32 + column_delta;
                    if next_row < 0
                        || next_column < 0
                        || next_row >= rows as i32
                        || next_column >= columns as i32
                    {
                        continue;
                    }
                    let next = next_row as u32 * columns + next_column as u32;
                    if hot[next as usize] && !seen[next as usize] {
                        seen[next as usize] = true;
                        queue.push_back(next);
                    }
                }
            }
        }
        let x = min_column * REGION_CELL;
        let y = min_row * REGION_CELL;
        regions.push(DiffRegion {
            x,
            y,
            width: ((max_column + 1) * REGION_CELL).min(width) - x,
            height: ((max_row + 1) * REGION_CELL).min(height) - y,
            pixels,
        });
    }
    regions.sort_by_key(|region| std::cmp::Reverse(region.pixels));
    regions.truncate(12);
    regions
}

fn draw_regions(overlay: &mut RgbaImage, regions: &[DiffRegion]) {
    for region in regions {
        let right = region.x + region.width - 1;
        let bottom = region.y + region.height - 1;
        for x in region.x..=right {
            overlay.put_pixel(x, region.y, Rgba([0, 230, 255, 255]));
            overlay.put_pixel(x, bottom, Rgba([0, 230, 255, 255]));
        }
        for y in region.y..=bottom {
            overlay.put_pixel(region.x, y, Rgba([0, 230, 255, 255]));
            overlay.put_pixel(right, y, Rgba([0, 230, 255, 255]));
        }
    }
}
