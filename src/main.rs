use embassy_time::{Duration, Timer};
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::sys::link_patches;
use esp_idf_svc::hal::task::block_on;

fn main() {
    // Necessary for linking patches to the runtime
    link_patches();

    // Initialize the logger
    EspLogger::initialize_default();

    // Run the asynchronous main function
    block_on(async_main());
}

async fn async_main() {
    loop {
        log::info!("Asynchronous task running...");
        Timer::after(Duration::from_secs(1)).await;
    }
}