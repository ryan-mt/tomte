//! `tomte completions` — emit a shell completion script for tomte's whole
//! command surface. Generated from the same clap definition that parses the
//! CLI, so the script can never drift from the real commands and flags.
//!
//! Install examples:
//!   bash:       tomte completions bash > ~/.local/share/bash-completion/completions/tomte
//!   zsh:        tomte completions zsh > "${fpath[1]}/_tomte"
//!   fish:       tomte completions fish > ~/.config/fish/completions/tomte.fish
//!   powershell: add `tomte completions powershell | Out-String | Invoke-Expression` to $PROFILE

use anyhow::Result;
use clap_complete::Shell;

pub fn run(shell: Shell, mut cmd: clap::Command) -> Result<()> {
    let name = cmd.get_name().to_string();
    clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
    Ok(())
}
