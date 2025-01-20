use std::cell::RefCell;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::once_lock::OnceLock;
use embassy_sync::pubsub::PubSubChannel;
use embassy_time::{Duration, Timer};
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::sys::link_patches;

use esp_idf_hal::prelude::*;

use esp_backtrace as _;
use esp_idf_hal::gpio::{AnyIOPin, IOPin, Input, Output, PinDriver};
use esp_println as _;

static BUTTON_PIN: OnceLock<&mut RefCell<PinDriver<AnyIOPin, Input>>> = OnceLock::new();
static LED_PIN: OnceLock<&mut RefCell<PinDriver<AnyIOPin, Output>>> = OnceLock::new();
static BUTTON_CHANNEL: OnceLock<&mut RefCell<PubSubChannel<CriticalSectionRawMutex, u32, 1, 2, 1>>> = OnceLock::new();

fn gpio_int_callback() {
    // Assert FLAG indicating a press button happened
    let channel = BUTTON_CHANNEL.get().unwrap();
    channel.borrow_mut().publish_immediate(1);

}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // Necessary for linking patches to the runtime
    link_patches();

    // Initialize the logger
    EspLogger::initialize_default();
    defmt::println!("Starting async main");

    let peripherals = Peripherals::take().unwrap();

    // Initialize the shared LED pin
    defmt::println!("Setting up LED Pin");
    let led_pin = peripherals.pins.gpio2;
    let led_pin_driver = Box::new(RefCell::new(PinDriver::output(led_pin.downgrade()).unwrap()));
    let _res = LED_PIN.init(Box::leak(led_pin_driver));
    defmt::println!("Done setting up LED Pin");

    // Initialize the shared LED pin
    defmt::println!("Setting up BUTTON Pin");
    let button_pin = peripherals.pins.gpio4;
    let button_pin_driver = Box::new(RefCell::new(PinDriver::input(button_pin.downgrade()).unwrap()));
    let _res = BUTTON_PIN.init(Box::leak(button_pin_driver));
    defmt::println!("Done setting up BUTTON Pin");

    // Initialize the pubsub channel for button pushes
    defmt::println!("Setting up BUTTON PubSub Channel");
    let button_channel = Box::new(RefCell::new(PubSubChannel::<CriticalSectionRawMutex, u32, 1, 2, 1>::new()));
    let _res = BUTTON_CHANNEL.init(Box::leak(button_channel));
    defmt::println!("Done setting up BUTTON PubSub Channel");

    // Run the asynchronous main function
    spawner.spawn(blinky()).unwrap();
    spawner.spawn(async_main()).unwrap();
}

#[embassy_executor::task]
async fn blinky() {
    {
        let led_pin = LED_PIN.get().await;
        defmt::println!("Setting up LED blinky task on pin {:?}", led_pin.borrow().pin());
    }
    loop {
        defmt::println!("Looping on LED blinky task");
        let led_pin = LED_PIN.get().await;
        led_pin.borrow_mut().set_high().unwrap();
        Timer::after(Duration::from_millis(1000)).await;
        let led_pin = LED_PIN.get().await;
        led_pin.borrow_mut().set_low().unwrap();
        Timer::after(Duration::from_millis(1000)).await;
    }
}

#[embassy_executor::task]
async fn async_main() {
    defmt::println!("Starting async loop");
    defmt::info!("Waiting for EspLog to be ready");
    loop {
        defmt::info!("Asynchronous task running...");
        Timer::after(Duration::from_secs(1)).await;
    }
}