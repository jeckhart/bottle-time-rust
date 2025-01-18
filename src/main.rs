use embassy_time::{Duration, Timer};
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::sys::link_patches;
use esp_idf_svc::hal::task::block_on;

use esp_backtrace as _;
use esp_println as _;

fn main() {
    // Necessary for linking patches to the runtime
    link_patches();

    // Initialize the logger
    EspLogger::initialize_default();

    // Run the asynchronous main function
    block_on(async_main());
}

async fn async_main() {
    defmt::println!("Starting async loop");
    defmt::info!("Waiting for EspLog to be ready");
    loop {
        defmt::info!("Asynchronous task running...");
        Timer::after(Duration::from_secs(1)).await;
    }
}