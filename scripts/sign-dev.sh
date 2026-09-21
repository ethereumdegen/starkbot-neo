#!/bin/sh
# Sign a development build with a stable identity, so macOS stops re-asking.
#
# Why this exists (05 §Keychain, §Risks): a Keychain item's ACL and a TCC grant
# are both keyed to the *code-signing requirement* of the process asking. An
# unsigned or ad-hoc-signed binary has no stable identity — its hash changes on
# every rebuild — so "Always Allow" never carries over, and the user is asked
# again for every credential on every launch. Signing with a real identity and
# a fixed `--identifier` gives every rebuild the same designated requirement,
# which is what makes one "Always Allow" stick, and what lets the Accessibility
# and Speech Recognition grants survive `cargo build`.
#
# Usage:
#   scripts/sign-dev.sh                       # sign target/debug/neo
#   scripts/sign-dev.sh target/release/neo    # sign something else
#
# The identity is picked up from NEO_SIGN_IDENTITY, or the first codesigning
# identity in the login Keychain. An Apple Development certificate is enough;
# a Developer ID is only needed for distribution.
set -eu

binary="${1:-target/debug/neo}"
identifier="${NEO_SIGN_IDENTIFIER:-com.starkbot.neo}"

if [ ! -f "$binary" ]; then
	echo "sign-dev: $binary does not exist — build it first" >&2
	exit 1
fi

identity="${NEO_SIGN_IDENTITY:-}"
if [ -z "$identity" ]; then
	identity=$(security find-identity -v -p codesigning |
		sed -n 's/^ *1) \([0-9A-F]*\) .*/\1/p' |
		head -n 1)
fi
if [ -z "$identity" ]; then
	cat >&2 <<-'MESSAGE'
		sign-dev: no codesigning identity found.

		Without one, macOS re-prompts for the Keychain on every rebuild. Either
		install an Apple Development certificate (Xcode › Settings › Accounts ›
		Manage Certificates › +), or accept one prompt per build.
	MESSAGE
	exit 1
fi

codesign --force --sign "$identity" --identifier "$identifier" --timestamp=none "$binary"
codesign --display --verbose=2 "$binary" 2>&1 | sed -n 's/^\(Identifier\|Authority\|TeamIdentifier\)=/  &/p'
echo "sign-dev: signed $binary as $identifier"
echo "sign-dev: the next Keychain prompt is the last one — choose Always Allow."
