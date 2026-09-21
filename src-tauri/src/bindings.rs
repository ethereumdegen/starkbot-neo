//! The generated half of the Rust ↔ TS contract (04 §14).
//!
//! `ui/src/bridge/generated.ts` is written from the view models by
//! [`tests::generated_bindings_are_committed`], and the same test fails when
//! the committed file no longer matches what the Rust types produce. That is
//! the whole point: a renamed field breaks the TypeScript build on the next
//! `npm run build` instead of drifting until a screen reads `undefined`.
//!
//! 04 §14 asks for `tauri-specta`-generated bindings and names the fallback
//! this module takes: "`ts-rs` for types + one hand-written `bridge/api.ts`
//! with the same names". `tauri-specta` is still a 2.0.0-rc that is not in the
//! lockfile; `ts-rs` is a release.
//!
//! Nothing is mirrored here. Every type in the generated file is the type the
//! bridge actually serialises — the ids, key states and health verdicts derive
//! `TS` where they are defined, so there is no second spelling of them to fall
//! out of step. The only names the generated file does not own are the ones
//! `ui/src/bridge/foreign.ts` holds, and it says there why.

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use std::path::{Path, PathBuf};

    use neo_agent::ax::{ActReport, TrustReport};
    use neo_agent::doctor::Health;
    use neo_core::{
        ConversationId, InferenceConnection, KeySource, KeyState, MessageId, MessageKind,
        MessageRole, ProviderAccountStatus, RunId,
    };
    use ts_rs::{Config, TS};

    use crate::error::UiError;
    use crate::view::{
        AxRequestView, AxResponseView, BootstrapView, CaseListingView, CheckView, ConnectionRow,
        ConversationView, Fix, InferenceView, KeyRow, LoginFailed, LoginStart, MessageView,
        ModelRow, RunKind, RunView, RuntimeOption, SettingsView,
    };

    /// The command that rewrites the committed file, quoted verbatim by the
    /// failure so nobody has to go looking for it.
    const REGENERATE: &str = "UPDATE_BINDINGS=1 cargo test -p neo-desktop bindings";

    /// The names the webview shares with types ts-rs is not the right tool
    /// for: the recursive `Json`, the settings record, and the accessibility
    /// observation, whose rows are omitted rather than nulled when empty —
    /// a shape ts-rs cannot express and TypeScript must get right.
    const IMPORTS: &str =
        "import type { AxAppView, AxTableView, Json, SettingsRecord } from \"./foreign\";\n\n";

    fn header() -> String {
        format!(
            "// Generated from `src-tauri/src/view.rs` by `cargo test -p neo-desktop`.\n\
             // Do not edit: regenerate with\n\
             //\n\
             //     {REGENERATE}\n\
             //\n\
             // `bindings::tests::generated_bindings_are_committed` fails while this\n\
             // file differs from what the Rust view models produce, so a renamed\n\
             // field is a red test and a broken `tsc`, never a silent `undefined`.\n\n"
        )
    }

    /// One `export type …`, with the Rust doc comment carried over as JSDoc.
    fn append<T: TS + ?Sized>(out: &mut String, cfg: &Config) {
        if let Some(docs) = T::docs() {
            out.push_str(docs.trim_start_matches('\n'));
        }
        out.push_str("export ");
        out.push_str(&T::decl(cfg));
        out.push_str("\n\n");
    }

    /// The whole file, in a fixed order: the leaf types the view models are
    /// spelled in, then the view models themselves.
    fn bindings() -> String {
        // 64-bit integers cross as JSON numbers, not `bigint`: everything on
        // this bridge goes through `serde_json` and Tauri's IPC, which writes
        // them as numbers. The values that use the width are millisecond
        // timestamps and second counts, nowhere near 2^53.
        let cfg = Config::new().with_large_int("number");
        let mut out = String::with_capacity(16 * 1024);
        out.push_str(&header());
        out.push_str(IMPORTS);
        out.push_str(&format!(
            "/** The protocol version this bundle was built against; `handshake` refuses a core that speaks another. */\nexport const BRIDGE_VERSION = {};\n\n",
            neo_agent::BRIDGE_VERSION
        ));

        append::<RunId>(&mut out, &cfg);
        append::<ConversationId>(&mut out, &cfg);
        append::<MessageId>(&mut out, &cfg);
        append::<Health>(&mut out, &cfg);
        append::<ProviderAccountStatus>(&mut out, &cfg);
        append::<KeyState>(&mut out, &cfg);
        append::<KeySource>(&mut out, &cfg);
        append::<InferenceConnection>(&mut out, &cfg);
        append::<MessageRole>(&mut out, &cfg);
        append::<MessageKind>(&mut out, &cfg);
        append::<TrustReport>(&mut out, &cfg);
        append::<ActReport>(&mut out, &cfg);

        append::<UiError>(&mut out, &cfg);
        append::<Fix>(&mut out, &cfg);
        append::<ConnectionRow>(&mut out, &cfg);
        append::<LoginStart>(&mut out, &cfg);
        append::<LoginFailed>(&mut out, &cfg);
        append::<KeyRow>(&mut out, &cfg);
        append::<RuntimeOption>(&mut out, &cfg);
        append::<InferenceView>(&mut out, &cfg);
        append::<CheckView>(&mut out, &cfg);
        append::<ModelRow>(&mut out, &cfg);
        append::<SettingsView>(&mut out, &cfg);
        append::<RunKind>(&mut out, &cfg);
        append::<RunView>(&mut out, &cfg);
        append::<ConversationView>(&mut out, &cfg);
        append::<MessageView>(&mut out, &cfg);
        append::<CaseListingView>(&mut out, &cfg);
        append::<BootstrapView>(&mut out, &cfg);
        append::<AxRequestView>(&mut out, &cfg);
        append::<AxResponseView>(&mut out, &cfg);
        out
    }

    fn generated() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui/src/bridge/generated.ts")
    }

    /// The first line that differs, because "the file changed" is not enough
    /// to act on and a whole-file dump is not readable in a test failure.
    fn first_difference(committed: &str, expected: &str) -> String {
        for (line, (old, new)) in committed.lines().zip(expected.lines()).enumerate() {
            if old != new {
                return format!("line {}:\n  committed: {old}\n  generated: {new}", line + 1);
            }
        }
        format!(
            "the first {} line(s) agree; committed has {}, generated has {}",
            committed.lines().count().min(expected.lines().count()),
            committed.lines().count(),
            expected.lines().count()
        )
    }

    /// The contract: what `view.rs` says and what `generated.ts` says are the
    /// same thing.
    #[test]
    fn generated_bindings_are_committed() {
        let expected = bindings();
        let path = generated();
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        if committed == expected {
            return;
        }
        if std::env::var_os("UPDATE_BINDINGS").is_some() {
            std::fs::write(&path, &expected).expect("the generated bindings are writable");
            return;
        }
        panic!(
            "ui/src/bridge/generated.ts is out of date with src-tauri/src/view.rs.\n\
             {}\n\nRegenerate it with:\n\n    {REGENERATE}\n",
            first_difference(&committed, &expected)
        );
    }

    /// `Fix` is the one view type whose TypeScript shape is load-bearing: the
    /// screens switch on `kind` to decide which control to offer, so the
    /// discriminated union serde produces has to be the union TypeScript sees.
    #[test]
    fn fix_is_a_discriminated_union_on_kind() {
        let decl = Fix::decl(&Config::new());
        for variant in [
            "\"kind\": \"set_key\", account: string",
            "\"kind\": \"sign_in\", provider: string",
            "\"kind\": \"choose_runtime\"",
            "\"kind\": \"manual\", detail: string",
        ] {
            assert!(decl.contains(variant), "`{variant}` is missing from {decl}");
        }
        let json = serde_json::to_value(Fix::SetKey {
            account: "typesafe".to_owned(),
        })
        .expect("a fix serialises");
        assert_eq!(json["kind"], "set_key");
        assert_eq!(json["account"], "typesafe");
    }

    /// The ids are `#[serde(transparent)]` newtypes over a Uuid, and the
    /// bindings say `string`. A front end handed `{ }` instead would index it
    /// forever, so the two are checked against each other rather than assumed.
    #[test]
    fn ids_cross_the_bridge_as_strings() {
        let cfg = Config::new();
        assert_eq!(RunId::decl(&cfg), "type RunId = string;");
        assert_eq!(ConversationId::decl(&cfg), "type ConversationId = string;");
        let run = RunId::new();
        assert_eq!(
            serde_json::to_value(run).expect("an id serialises"),
            serde_json::Value::String(run.to_string())
        );
    }
}
