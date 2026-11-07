/// GUI file synchronization client.
#[derive(clap::Parser)]
#[command(version)]
struct Cli {
    /// Start in the system tray without opening the main window, as the start at login does.
    #[arg(long)]
    hidden: bool,
    /// Open the main window on startup, which is the default now; kept for menu and autostart entries written before `--hidden`.
    #[arg(long, hide = true)]
    show: bool,
    /// Save these files in the cloud: Syncative asks for the account and the folder, then uploads them in the background. The file manager's context menu uses it.
    #[arg(long, num_args = 1.., value_name = "FILE")]
    save: Vec<std::path::PathBuf>,
}

fn main() {
    let cli = <Cli as clap::Parser>::parse();
    // The running Syncative may have another working directory.
    let save = cli.save.into_iter().map(|path| std::path::absolute(&path).unwrap_or(path)).collect();
    syncative::start_saving(!cli.hidden || cli.show, save);
}
