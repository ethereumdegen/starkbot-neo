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
//!
//! Two things a generator cannot write are pinned here too, because a
//! generated file is only worth what fails when it is stale.
//! [`tests::every_command_is_spelled_the_same_on_both_sides`] holds the
//! command names — spelled by hand in `generate_handler!` and again in
//! `api.ts`, where a rename on one side is a runtime "command not found" —
//! and [`tests::a_changed_declaration_bumps_the_bridge_version`] holds
//! `BRIDGE_VERSION` against a digest of the declarations, so regenerating
//! this file can no longer leave the handshake agreeing about a protocol
//! that changed underneath it.

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

    /// The whole file: the header, the version, then every declaration.
    fn bindings() -> String {
        let mut out = String::with_capacity(16 * 1024);
        out.push_str(&header());
        out.push_str(IMPORTS);
        out.push_str(&format!(
            "/** The protocol version this bundle was built against; `handshake` refuses a core that speaks another. */\nexport const BRIDGE_VERSION = {};\n\n",
            neo_agent::BRIDGE_VERSION
        ));
        out.push_str(&declarations());
        out
    }

    /// Every type the bridge carries, in a fixed order: the leaf types the
    /// view models are spelled in, then the view models themselves.
    ///
    /// Separate from the file around it because this is what
    /// [`a_changed_declaration_bumps_the_bridge_version`] digests. The
    /// header is a comment and the version line is the thing being checked;
    /// hashing either would make the pin churn on a copy edit.
    fn declarations() -> String {
        // 64-bit integers cross as JSON numbers, not `bigint`: everything on
        // this bridge goes through `serde_json` and Tauri's IPC, which writes
        // them as numbers. The values that use the width are millisecond
        // timestamps and second counts, nowhere near 2^53.
        let cfg = Config::new().with_large_int("number");
        let mut out = String::with_capacity(16 * 1024);

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

    /// FNV-1a, spelled out rather than taken from `DefaultHasher`.
    ///
    /// The value below is committed, and `DefaultHasher`'s output is
    /// explicitly not stable across releases — a toolchain bump would read
    /// as a protocol change and the failure would say the opposite of what
    /// happened.
    fn digest(text: &str) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in text.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    /// The declarations this bridge last published, and the
    /// `BRIDGE_VERSION` they went out as.
    ///
    /// `UPDATE_BINDINGS=1` rewrites `generated.ts`, and that is exactly why
    /// the pin lives here instead: regenerating never bumped the version, so
    /// a `ui/dist` built one field-rename ago still reported `2` and passed
    /// `handshake` — the skew the handshake exists to catch. Both halves
    /// move together or the test below is red.
    const PUBLISHED: (u64, u32) = (0x7bda_f52d_52bf_9c55, 2);

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
    /// discriminated union serde produces has to be the union TypeScript
    /// sees. What the *serialiser* does is the claim worth making; the
    /// generated text is already pinned, whole, by
    /// [`generated_bindings_are_committed`].
    #[test]
    fn fix_is_a_discriminated_union_on_kind() {
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

    /// A declaration that changed without the version moving is the skew
    /// `handshake` exists to catch: `ui/dist` outlives the binary it was
    /// built for, and regenerating the bindings never bumped anything.
    #[test]
    fn a_changed_declaration_bumps_the_bridge_version() {
        let current = digest(&declarations());
        assert_eq!(
            (current, neo_agent::BRIDGE_VERSION),
            PUBLISHED,
            "the bridge declarations or its version moved.\n\n\
             Bump `neo_agent::BRIDGE_VERSION`, run `{REGENERATE}`, and set\n\n    \
             const PUBLISHED: (u64, u32) = ({current:#018x}, <the new version>);\n"
        );
    }

    /// The command names `generate_handler!` registers.
    ///
    /// Read out of the source because the macro leaves nothing to ask at
    /// runtime: it expands to a `match` on `&str` inside Tauri's invoke
    /// handler, and a name it does not have is a rejected message, not a
    /// compile error.
    fn registered_commands() -> Vec<String> {
        let source = include_str!("main.rs");
        let (_, rest) = source
            .split_once("generate_handler![")
            .expect("main.rs registers its commands with `generate_handler!`");
        let (list, _) = rest.split_once(']').expect("the handler list is closed");
        let mut names: Vec<String> = list
            .split(',')
            .filter_map(|entry| entry.trim().strip_prefix("commands::"))
            .map(str::to_owned)
            .collect();
        names.sort_unstable();
        names
    }

    /// The command names `ui/src/bridge/api.ts` invokes.
    fn invoked_commands() -> Vec<String> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui/src/bridge/api.ts");
        let source = std::fs::read_to_string(&path).expect("ui/src/bridge/api.ts is readable");
        // Every call is `invoke<T>("name", …)`, which is what makes the
        // names findable at all. One written without the turbofish would be
        // invisible to this parse and so pass a check it never entered.
        assert!(
            !source.contains("invoke("),
            "every command in api.ts must be called as `invoke<T>(\"name\", …)`"
        );
        let mut names: Vec<String> = source
            .split("invoke<")
            .skip(1)
            .filter_map(|tail| tail.split_once(">(\""))
            .filter_map(|(_, rest)| rest.split_once('"'))
            .map(|(name, _)| name.to_owned())
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// The half of the bridge no generator produces.
    ///
    /// `bindings.rs` pins every type that crosses; the command *names* are
    /// spelled twice by hand, and renaming one side alone is a runtime
    /// "command not found" — a dead button, found by a user rather than by
    /// a build.
    #[test]
    fn every_command_is_spelled_the_same_on_both_sides() {
        assert_eq!(
            registered_commands(),
            invoked_commands(),
            "`generate_handler!` in src-tauri/src/main.rs and `invoke` in \
             ui/src/bridge/api.ts name different commands"
        );
    }
}
