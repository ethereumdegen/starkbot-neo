use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Author {
    User,
    Agent,
    System,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: String,
    pub parent: Option<String>,
    pub text: String,
    pub styles: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pin {
    pub id: String,
    pub node: String,
    pub text: String,
    pub anchor: (f32, f32),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    SetText {
        node: String,
        text: String,
    },
    SetStyle {
        node: String,
        property: String,
        value: Option<String>,
    },
    KnobSet {
        name: String,
        value: String,
    },
    PinAdd {
        node: String,
        text: String,
        anchor: (f32, f32),
    },
    PinRemove {
        id: String,
    },
    Rewrite {
        root: String,
        changes: Vec<(String, String)>,
    },
}

#[derive(Debug, Clone)]
pub struct Txn {
    pub id: u64,
    pub author: Author,
    pub label: String,
    pub ops: Vec<Op>,
    pub inverses: Vec<Op>,
    pub footprint: BTreeSet<String>,
    pub rev_before: u64,
    pub rev_after: u64,
    pub dropped: Vec<String>,
    pub undone: bool,
}

#[derive(Debug, Clone)]
pub struct CanvasDoc {
    pub rev: u64,
    pub nodes: BTreeMap<String, Node>,
    pub knobs: BTreeMap<String, String>,
    pub pins: BTreeMap<String, Pin>,
    pub log: Vec<Txn>,
    next_txn: u64,
    next_pin: u64,
}

impl CanvasDoc {
    pub fn launch_card() -> Self {
        let mut nodes = BTreeMap::new();
        for (id, parent, text) in [
            ("n1", None, ""),
            ("n2", Some("n1"), ""),
            ("n3", Some("n1"), "Degen Radio · launch week"),
            ("n4", Some("n1"), "Charts that actually slap."),
            ("n5", Some("n1"), "Listen now"),
        ] {
            nodes.insert(
                id.into(),
                Node {
                    id: id.into(),
                    parent: parent.map(str::to_owned),
                    text: text.into(),
                    styles: BTreeMap::new(),
                },
            );
        }
        Self {
            rev: 0,
            nodes,
            knobs: BTreeMap::from([
                ("--k-headline".into(), "112px".into()),
                ("--k-pad".into(), "72px".into()),
            ]),
            pins: BTreeMap::new(),
            log: Vec::new(),
            next_txn: 1,
            next_pin: 1,
        }
    }

    pub fn commit(
        &mut self,
        author: Author,
        label: impl Into<String>,
        ops: Vec<Op>,
    ) -> Result<&Txn, String> {
        let before = self.clone();
        let rev_before = self.rev;
        let mut inverses = Vec::with_capacity(ops.len());
        let mut footprint = BTreeSet::new();
        for op in &ops {
            footprint.extend(Self::footprint(op));
            match self.apply_one(op) {
                Ok(inverse) => inverses.push(inverse),
                Err(error) => {
                    *self = before;
                    return Err(error);
                }
            }
        }
        self.rev += 1;
        let txn = Txn {
            id: self.next_txn,
            author,
            label: label.into(),
            ops,
            inverses,
            footprint,
            rev_before,
            rev_after: self.rev,
            dropped: Vec::new(),
            undone: false,
        };
        self.next_txn += 1;
        self.log.push(txn);
        Ok(self.log.last().expect("transaction just pushed"))
    }

    pub fn undo(&mut self, author: Author) -> Result<&Txn, String> {
        let target = self
            .log
            .iter()
            .rposition(|txn| txn.author == author && !txn.undone)
            .ok_or_else(|| "nothing to undo".to_owned())?;
        let target_footprint = self.log[target].footprint.clone();
        let later: BTreeSet<String> = self.log[target + 1..]
            .iter()
            .filter(|txn| !txn.undone)
            .flat_map(|txn| txn.footprint.iter().cloned())
            .collect();
        let mut inverses = Vec::new();
        let mut dropped = Vec::new();
        for inverse in self.log[target].inverses.clone().into_iter().rev() {
            let inverse = match inverse {
                Op::Rewrite { root, changes } => {
                    let (kept, skipped): (Vec<_>, Vec<_>) = changes
                        .into_iter()
                        .partition(|(node, _)| !later.contains(&format!("node:{node}:text")));
                    dropped.extend(
                        skipped
                            .into_iter()
                            .map(|(node, _)| format!("node:{node}:text")),
                    );
                    if kept.is_empty() {
                        continue;
                    }
                    Op::Rewrite {
                        root,
                        changes: kept,
                    }
                }
                inverse => {
                    let footprint = Self::footprint(&inverse);
                    if footprint.iter().any(|key| later.contains(key)) {
                        dropped.extend(footprint);
                        continue;
                    }
                    inverse
                }
            };
            self.apply_one(&inverse)?;
            inverses.push(inverse);
        }
        self.log[target].undone = true;
        self.rev += 1;
        let txn = Txn {
            id: self.next_txn,
            author: Author::System,
            label: format!("undo: {}", self.log[target].label),
            ops: inverses,
            inverses: Vec::new(),
            footprint: target_footprint,
            rev_before: self.rev - 1,
            rev_after: self.rev,
            dropped,
            undone: false,
        };
        self.next_txn += 1;
        self.log.push(txn);
        Ok(self.log.last().expect("undo transaction just pushed"))
    }

    fn apply_one(&mut self, op: &Op) -> Result<Op, String> {
        match op {
            Op::SetText { node, text } => {
                let target = self
                    .nodes
                    .get_mut(node)
                    .ok_or_else(|| format!("unknown node {node}"))?;
                let old = std::mem::replace(&mut target.text, text.clone());
                Ok(Op::SetText {
                    node: node.clone(),
                    text: old,
                })
            }
            Op::SetStyle {
                node,
                property,
                value,
            } => {
                let target = self
                    .nodes
                    .get_mut(node)
                    .ok_or_else(|| format!("unknown node {node}"))?;
                let old = target.styles.get(property).cloned();
                match value {
                    Some(value) => {
                        target.styles.insert(property.clone(), value.clone());
                    }
                    None => {
                        target.styles.remove(property);
                    }
                }
                Ok(Op::SetStyle {
                    node: node.clone(),
                    property: property.clone(),
                    value: old,
                })
            }
            Op::KnobSet { name, value } => {
                if !name.starts_with("--k-") {
                    return Err("knob names must start with --k-".into());
                }
                let old = self
                    .knobs
                    .insert(name.clone(), value.clone())
                    .unwrap_or_default();
                Ok(Op::KnobSet {
                    name: name.clone(),
                    value: old,
                })
            }
            Op::PinAdd { node, text, anchor } => {
                if !self.nodes.contains_key(node) {
                    return Err(format!("unknown node {node}"));
                }
                let id = format!("p{}", self.next_pin);
                self.next_pin += 1;
                self.pins.insert(
                    id.clone(),
                    Pin {
                        id: id.clone(),
                        node: node.clone(),
                        text: text.clone(),
                        anchor: *anchor,
                    },
                );
                Ok(Op::PinRemove { id })
            }
            Op::PinRemove { id } => {
                let pin = self
                    .pins
                    .remove(id)
                    .ok_or_else(|| format!("unknown pin {id}"))?;
                Ok(Op::PinAdd {
                    node: pin.node,
                    text: pin.text,
                    anchor: pin.anchor,
                })
            }
            Op::Rewrite { root, changes } => {
                if !self.nodes.contains_key(root) {
                    return Err(format!("unknown rewrite root {root}"));
                }
                if changes
                    .iter()
                    .any(|(node, _)| !self.is_descendant(node, root))
                {
                    return Err("rewrite escaped its leased subtree".into());
                }
                let mut old = Vec::with_capacity(changes.len());
                for (node, text) in changes {
                    let target = self
                        .nodes
                        .get_mut(node)
                        .ok_or_else(|| format!("unknown node {node}"))?;
                    old.push((
                        node.clone(),
                        std::mem::replace(&mut target.text, text.clone()),
                    ));
                }
                Ok(Op::Rewrite {
                    root: root.clone(),
                    changes: old,
                })
            }
        }
    }

    fn is_descendant(&self, node: &str, root: &str) -> bool {
        let mut current = Some(node);
        while let Some(id) = current {
            if id == root {
                return true;
            }
            current = self.nodes.get(id).and_then(|node| node.parent.as_deref());
        }
        false
    }

    fn footprint(op: &Op) -> BTreeSet<String> {
        match op {
            Op::SetText { node, .. } => BTreeSet::from([format!("node:{node}:text")]),
            Op::SetStyle { node, property, .. } => {
                BTreeSet::from([format!("node:{node}:style:{property}")])
            }
            Op::KnobSet { name, .. } => BTreeSet::from([format!("knob:{name}")]),
            Op::PinAdd { node, .. } => BTreeSet::from([format!("node:{node}:pins")]),
            Op::PinRemove { id } => BTreeSet::from([format!("pin:{id}")]),
            Op::Rewrite { changes, .. } => changes
                .iter()
                .map(|(node, _)| format!("node:{node}:text"))
                .collect(),
        }
    }

    pub fn frame_html(&self) -> String {
        let text = |id: &str| escape(&self.nodes[id].text);
        let style = |id: &str| {
            self.nodes[id]
                .styles
                .iter()
                .map(|(key, value)| format!("{key}:{value}"))
                .collect::<Vec<_>>()
                .join(";")
        };
        let knobs = self
            .knobs
            .iter()
            .map(|(key, value)| format!("{key}:{value}"))
            .collect::<Vec<_>>()
            .join(";");
        format!(
            r##"<!doctype html><html data-neo-frame="f1" data-kind="graphic" data-v="1"><head><meta charset="utf-8"><style>
:root{{{knobs};--bg:#0b1020;--accent:#7c5cff;--ink:#f4f6ff}}*{{box-sizing:border-box}}html,body{{margin:0;background:transparent}}
.frame{{width:1080px;height:1080px;padding:var(--k-pad);background:radial-gradient(1200px 700px at 80% 0%,#2a1f6b 0%,var(--bg) 60%);color:var(--ink);font-family:-apple-system,system-ui,sans-serif;display:flex;flex-direction:column;justify-content:space-between;overflow:hidden;position:relative}}
.kicker{{font-size:30px;letter-spacing:.18em;text-transform:uppercase;opacity:.75}}.headline{{font-size:var(--k-headline);line-height:.98;margin:0;font-weight:800;letter-spacing:-.03em;max-width:850px}}
.cta{{align-self:flex-start;font-size:38px;font-weight:700;padding:26px 46px;border-radius:36px;background:var(--accent);color:#fff}}.orb{{position:absolute;right:-140px;bottom:-140px;width:560px;height:560px;border-radius:50%;background:conic-gradient(var(--accent),#22d3ee,#f472b6,var(--accent));filter:blur(8px);opacity:.85}}
[data-n="n2"]{{{}}}[data-n="n3"]{{{}}}[data-n="n4"]{{{}}}[data-n="n5"]{{{}}}
</style></head><body data-n="n1"><main class="frame"><div class="orb" data-n="n2"></div><div class="kicker" data-n="n3">{}</div><h1 class="headline" data-n="n4">{}</h1><div class="cta" data-n="n5">{}</div></main></body></html>"##,
            style("n2"),
            style("n3"),
            style("n4"),
            style("n5"),
            text("n3"),
            text("n4"),
            text("n5")
        )
    }
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_inverse_is_byte_identical() {
        let mut doc = CanvasDoc::launch_card();
        let before = doc.frame_html();
        doc.commit(
            Author::User,
            "edit",
            vec![Op::SetText {
                node: "n4".into(),
                text: "New".into(),
            }],
        )
        .unwrap();
        doc.undo(Author::User).unwrap();
        assert_eq!(doc.frame_html(), before);
    }

    #[test]
    fn later_user_property_wins_over_agent_undo() {
        let mut doc = CanvasDoc::launch_card();
        doc.commit(
            Author::Agent,
            "rewrite",
            vec![Op::Rewrite {
                root: "n1".into(),
                changes: vec![
                    ("n4".into(), "Agent".into()),
                    ("n5".into(), "Agent CTA".into()),
                ],
            }],
        )
        .unwrap();
        doc.commit(
            Author::User,
            "copy",
            vec![Op::SetText {
                node: "n4".into(),
                text: "User".into(),
            }],
        )
        .unwrap();
        let dropped = doc.undo(Author::Agent).unwrap().dropped.clone();
        assert_eq!(doc.nodes["n4"].text, "User");
        assert_eq!(doc.nodes["n5"].text, "Listen now");
        assert_eq!(dropped, vec!["node:n4:text"]);
    }
}
