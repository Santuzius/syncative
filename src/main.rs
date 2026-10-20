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
}

fn main() {
    let cli = <Cli as clap::Parser>::parse();
    syncative::start(!cli.hidden || cli.show);
}
