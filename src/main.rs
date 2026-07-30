use openaction::OpenActionResult;

mod app_column;
mod application_targets;
mod audio;
mod device_dial;
mod dial;
mod dynamic;
mod gfx;
mod icons;
mod mixer;
mod plugin;
mod utils;

#[tokio::main]
async fn main() -> OpenActionResult<()> {
    simplelog::TermLogger::init(
        simplelog::LevelFilter::Debug,
        simplelog::Config::default(),
        simplelog::TerminalMode::Stdout,
        simplelog::ColorChoice::Never,
    )
    .unwrap();

    println!("Starting Volume Controller plugin...");

    plugin::init().await
}
