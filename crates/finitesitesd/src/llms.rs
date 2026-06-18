//! Generated `llms.txt` guidance for agent-editable Finite Sites.
//!
//! This is platform guidance, not site content. The serving plane only emits
//! it when the active Version has no user-authored `/llms.txt`.

const FSITE_REPOSITORY_URL: &str = env!("CARGO_PKG_REPOSITORY");

pub fn generated_llms_txt(site_name: &str, site_url: &str, api_url: &str) -> String {
    assert!(!site_name.is_empty());
    assert!(!site_url.is_empty());
    assert!(!api_url.is_empty());
    format!(
        "\
# Finite Sites Editing Instructions

This site is editable through Finite Sites. Use these instructions when a human asks you to make a change to this site.

Site name: {site_name}
Site URL: {site_url}
API URL: {api_url}

Use the editor email address the human gave you. Do not guess an email address, and do not publish with a different identity.

Install the fsite CLI:

- Download the latest release from {FSITE_REPOSITORY_URL}/releases/latest
- Release assets are named fsite-linux-x86_64.tar.gz, fsite-macos-x86_64.tar.gz, and fsite-macos-aarch64.tar.gz
- Or build from source with: cargo install --git {FSITE_REPOSITORY_URL} --package fsite-cli --bin fsite

Configure the API:

export FINITE_SITES_API=\"{api_url}\"

Verify this machine for the editor email:

fsite email-login YOUR_EDITOR_EMAIL
fsite email-redeem YOUR_EDITOR_EMAIL TOKEN_FROM_EMAIL

Pull the source snapshot, edit it, and publish the updated source:

fsite source pull {site_name} ./site-source --email YOUR_EDITOR_EMAIL
cd ./site-source
# make the requested changes
# run the project's tests and build command when discoverable
# replace ./dist with the actual static output directory if this project uses another one
fsite publish {site_name} ./dist --source . --email YOUR_EDITOR_EMAIL

Rules:

- Pull the source snapshot before editing. Do not reconstruct the source from rendered HTML unless source pull is unavailable.
- Publish the built/static artifact directory, not `.git`, `.finite`, dependency directories, or local secret files.
- Preserve a user-authored llms.txt if the project contains one.
- Publish with --source so the next editor receives the updated source.
- If authentication or authorization fails, ask the human to confirm the editor email grant.
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_text_has_actionable_commands_without_private_emails() {
        let text = generated_llms_txt(
            "demo",
            "https://demo.finite.chat/",
            "https://api.finite.chat",
        );

        assert!(text.contains("Site name: demo"));
        assert!(text.contains("Site URL: https://demo.finite.chat/"));
        assert!(text.contains("fsite email-login YOUR_EDITOR_EMAIL"));
        assert!(text.contains("fsite source pull demo ./site-source --email YOUR_EDITOR_EMAIL"));
        assert!(text.contains("https://github.com/finitecomputer/finite-sites/releases/latest"));
        assert!(!text.contains("skyler"));
        assert!(!text.contains("paul"));
    }
}
