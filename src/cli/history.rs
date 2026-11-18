//! `voxtype history` subcommands.

use clap::Subcommand;

#[derive(Subcommand, Debug, Clone)]
pub enum HistoryAction {
    /// List recent dictations, newest first
    List {
        /// How many entries to show
        #[arg(long, short = 'n', default_value_t = 20)]
        limit: usize,

        /// Print a JSON array instead of one line per entry
        #[arg(long)]
        json: bool,
    },

    /// Print the most recent dictation's text
    Last {
        /// Print the full entry as JSON
        #[arg(long)]
        json: bool,
    },

    /// Delete every stored dictation
    Clear,
}
