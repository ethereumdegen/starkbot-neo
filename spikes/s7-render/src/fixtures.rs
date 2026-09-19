use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Fixture {
    pub id: &'static str,
    pub html: &'static str,
}

pub const METRICS_EXPRESSION: &str = r#"[...doc.querySelectorAll('[data-n]')].map(el=>{const r=el.getBoundingClientRect(),s=getComputedStyle(el);return{id:el.dataset.n,x:r.x,y:r.y,width:r.width,height:r.height,fontSize:s.fontSize,lineHeight:s.lineHeight,display:s.display,scrollWidth:el.scrollWidth,scrollHeight:el.scrollHeight}})"#;

pub fn engine_drift_fixtures() -> Vec<Fixture> {
    vec![
        Fixture {
            id: "flex-launch",
            html: FLEX_LAUNCH,
        },
        Fixture {
            id: "grid-pricing",
            html: GRID_PRICING,
        },
        Fixture {
            id: "type-rhythm",
            html: TYPE_RHYTHM,
        },
        Fixture {
            id: "container-card",
            html: CONTAINER_CARD,
        },
        Fixture {
            id: "svg-aspect",
            html: SVG_ASPECT,
        },
    ]
}

const FLEX_LAUNCH: &str = r#"<!doctype html><html><head><meta charset="utf-8"><style>
*{box-sizing:border-box}html,body{margin:0;width:100%;height:100%;overflow:hidden}body{font-family:-apple-system,BlinkMacSystemFont,'Helvetica Neue',sans-serif;color:#f7f7fb;background:#0b1020}
.frame{width:1080px;height:1080px;padding:72px;display:flex;flex-direction:column;justify-content:space-between;background:radial-gradient(800px 600px at 90% 80%,#744cff,#0b1020 70%)}
.kicker{font-size:28px;line-height:1.2;letter-spacing:.18em;text-transform:uppercase}.copy{display:flex;flex-direction:column;gap:28px;max-width:850px}.copy h1{font-size:112px;line-height:.94;letter-spacing:-.04em;margin:0}.cta{align-self:flex-start;padding:24px 42px;border-radius:999px;background:#7c5cff;font-size:34px;line-height:1.1;font-weight:700}
</style></head><body data-n="f1-root"><main class="frame" data-n="f1-frame"><div class="kicker" data-n="f1-kicker">Degen Radio · launch week</div><section class="copy" data-n="f1-copy"><h1 data-n="f1-title">Radio without the noise.</h1><div class="cta" data-n="f1-cta">Listen now</div></section></main></body></html>"#;

const GRID_PRICING: &str = r#"<!doctype html><html><head><meta charset="utf-8"><style>
*{box-sizing:border-box}html,body{margin:0;width:100%;height:100%;overflow:hidden}body{font-family:-apple-system,BlinkMacSystemFont,'Helvetica Neue',sans-serif;color:#15151b;background:#f3f0ff;padding:64px}.frame{width:952px;height:952px;display:grid;grid-template-rows:auto 1fr;gap:42px}.head{display:grid;grid-template-columns:1.4fr .6fr;align-items:end;gap:32px}.head h1{font-size:76px;line-height:.95;letter-spacing:-.045em;margin:0}.head p{font-size:22px;line-height:1.45;margin:0}.plans{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:20px}.plan{display:grid;grid-template-rows:auto auto 1fr auto;gap:22px;padding:30px;border:1px solid #c9c2e8;border-radius:28px;background:#fff}.plan.hot{background:#171228;color:#fff;transform:translateY(-16px)}.price{font-size:58px;font-weight:800;letter-spacing:-.04em}.features{display:flex;flex-direction:column;gap:12px;padding:0;margin:0;list-style:none}.button{padding:16px 20px;border-radius:14px;text-align:center;background:#7c5cff;color:white;font-weight:700}
</style></head><body data-n="f2-root"><main class="frame" data-n="f2-frame"><header class="head" data-n="f2-head"><h1 data-n="f2-title">Pick your signal.</h1><p data-n="f2-sub">Every plan includes live charts and zero ads.</p></header><section class="plans" data-n="f2-plans"><article class="plan" data-n="f2-a"><strong>Listener</strong><div class="price" data-n="f2-ap">$0</div><ul class="features"><li>Live radio</li><li>Basic charts</li></ul><div class="button">Start</div></article><article class="plan hot" data-n="f2-b"><strong>Trader</strong><div class="price" data-n="f2-bp">$12</div><ul class="features"><li>Every chart</li><li>Signal alerts</li><li>Archive</li></ul><div class="button">Go pro</div></article><article class="plan" data-n="f2-c"><strong>Desk</strong><div class="price" data-n="f2-cp">$49</div><ul class="features"><li>Five seats</li><li>API access</li></ul><div class="button">Talk to us</div></article></section></main></body></html>"#;

const TYPE_RHYTHM: &str = r#"<!doctype html><html><head><meta charset="utf-8"><style>
*{box-sizing:border-box}html,body{margin:0;width:100%;height:100%;overflow:hidden}body{font-family:Georgia,'Times New Roman',serif;color:#191710;background:#f5efe2;padding:86px}.frame{width:908px;height:908px;display:flex;flex-direction:column}.eyebrow{font-family:-apple-system,sans-serif;font-size:18px;line-height:24px;letter-spacing:.24em;text-transform:uppercase;border-bottom:1px solid #8a806b;padding-bottom:18px}.headline{font-size:104px;line-height:.91;letter-spacing:-.055em;font-weight:400;max-width:840px;margin:54px 0 38px}.dek{font-size:30px;line-height:1.38;max-width:690px;margin:0}.footer{margin-top:auto;display:flex;justify-content:space-between;align-items:baseline;font-family:-apple-system,sans-serif;font-size:18px}.mark{font-size:44px;font-weight:800;letter-spacing:-.06em}
</style></head><body data-n="f3-root"><article class="frame" data-n="f3-frame"><div class="eyebrow" data-n="f3-eye">Field Notes · No. 18</div><h1 class="headline" data-n="f3-title">The shape of useful attention</h1><p class="dek" data-n="f3-dek">Interfaces become quiet when every element knows what it is responsible for.</p><footer class="footer" data-n="f3-foot"><span class="mark" data-n="f3-mark">NEO</span><span>September 2026</span></footer></article></body></html>"#;

const CONTAINER_CARD: &str = r#"<!doctype html><html><head><meta charset="utf-8"><style>
*{box-sizing:border-box}html,body{margin:0;width:100%;height:100%;overflow:hidden}body{font-family:-apple-system,BlinkMacSystemFont,'Helvetica Neue',sans-serif;background:#dce9ff;padding:80px;color:#102044}.shell{container-type:inline-size;width:920px;height:920px;border-radius:44px;background:white;padding:48px}.card{height:100%;display:grid;grid-template-columns:1.05fr .95fr;gap:44px;align-items:center}.art{aspect-ratio:1;border-radius:32px;background:linear-gradient(135deg,#7c5cff,#20d9d2);display:grid;place-items:center}.disc{width:62%;aspect-ratio:1;border-radius:50%;background:repeating-radial-gradient(circle,#101020 0 12px,#24243c 13px 18px)}.copy{display:flex;flex-direction:column;gap:24px}.copy h1{font-size:70px;line-height:.96;letter-spacing:-.05em;margin:0}.copy p{font-size:24px;line-height:1.4;margin:0}.tags{display:flex;flex-wrap:wrap;gap:10px}.tag{padding:10px 14px;border-radius:99px;background:#edf1ff;font-weight:650}@container (max-width:700px){.card{grid-template-columns:1fr}.art{max-height:420px}.copy h1{font-size:54px}}
</style></head><body data-n="f4-root"><main class="shell" data-n="f4-shell"><section class="card" data-n="f4-card"><div class="art" data-n="f4-art"><div class="disc" data-n="f4-disc"></div></div><div class="copy" data-n="f4-copy"><h1 data-n="f4-title">One room. Every signal.</h1><p data-n="f4-dek">A live desk for people who think in sound and charts.</p><div class="tags" data-n="f4-tags"><span class="tag">Live</span><span class="tag">Spatial audio</span><span class="tag">No ads</span></div></div></section></main></body></html>"#;

const SVG_ASPECT: &str = r#"<!doctype html><html><head><meta charset="utf-8"><style>
*{box-sizing:border-box}html,body{margin:0;width:100%;height:100%;overflow:hidden}body{font-family:-apple-system,BlinkMacSystemFont,'Helvetica Neue',sans-serif;background:#0c0c11;color:#fff;padding:70px}.frame{width:940px;height:940px;position:relative;display:grid;grid-template-rows:1fr auto;overflow:hidden;border-radius:48px;background:#161622}.visual{position:relative;display:grid;place-items:center}.visual svg{width:620px;aspect-ratio:4/3;filter:drop-shadow(0 30px 50px #0008);transform:rotate(-6deg)}.caption{display:flex;justify-content:space-between;align-items:end;padding:46px 52px}.caption h1{font-size:64px;line-height:1;margin:0;letter-spacing:-.045em}.caption span{font-size:20px;color:#aaa}.badge{position:absolute;right:52px;top:52px;width:116px;aspect-ratio:1;border-radius:50%;display:grid;place-items:center;background:#f3db62;color:#181610;font-size:19px;font-weight:800;transform:rotate(9deg)}
</style></head><body data-n="f5-root"><main class="frame" data-n="f5-frame"><section class="visual" data-n="f5-visual"><svg data-n="f5-svg" viewBox="0 0 400 300" role="img" aria-label="Abstract signal"><defs><linearGradient id="g" x2="1" y2="1"><stop stop-color="rgb(124 92 255)"/><stop offset="1" stop-color="rgb(35 213 208)"/></linearGradient></defs><path data-n="f5-path" fill="url(#g)" d="M20 220 C80 30 150 280 210 100 S330 10 380 180 L380 280 L20 280Z"/><circle cx="270" cy="95" r="46" fill="rgb(244 114 182)"/></svg><div class="badge" data-n="f5-badge">LIVE</div></section><footer class="caption" data-n="f5-caption"><h1 data-n="f5-title">Signal study 04</h1><span>1080 × 1080</span></footer></main></body></html>"#;
