use std::cell::RefCell;
use std::time::SystemTime;
use chrono::{DateTime, Utc};
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::once_lock::OnceLock;
use embassy_sync::pubsub::PubSubChannel;
use embassy_time::{Duration, Timer};
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::sys::link_patches;

use esp_idf_hal::prelude::*;

use esp_backtrace as _;
use esp_idf_hal::gpio::{AnyIOPin, IOPin, Input, InterruptType, Output, PinDriver, Pull};
use esp_idf_hal::task::queue::Queue;
use esp_println as _;

static BUTTON_PIN: OnceLock<&mut RefCell<PinDriver<AnyIOPin, Input>>> = OnceLock::new();
static LED_PIN: OnceLock<&mut RefCell<PinDriver<AnyIOPin, Output>>> = OnceLock::new();
static BUTTON_CHANNEL: OnceLock<&mut RefCell<PubSubChannel<CriticalSectionRawMutex, u32, 1, 2, 1>>> = OnceLock::new();
static BUTTON_QUEUE: OnceLock<&mut RefCell<Queue<i64>>> = OnceLock::new();

// fn gpio_int_callback() {
//     // Assert FLAG indicating a press button happened
//     let channel_future = BUTTON_CHANNEL.get().with_timeout(Duration::from_millis(1000));
//     let timestamp_ms = chrono::Local::now().timestamp_millis();
//     let res = BUTTON_QUEUE.send_back(timestamp_ms, 0).unwrap();
//     defmt::println!("Button pressed - {:?}", timestamp_ms);
// }

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // Necessary for linking patches to the runtime
    link_patches();

    // Initialize the logger
    EspLogger::initialize_default();

    let current_time = chrono::Local::now();
    let datetime: DateTime<Utc> = current_time.into();
    defmt::println!("Starting async main - current time {:?}", datetime.to_rfc3339().as_str());

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
    let mut button_driver = PinDriver::input(button_pin.downgrade()).unwrap();
    button_driver.set_pull(Pull::Up).unwrap();
    button_driver.set_interrupt_type(InterruptType::AnyEdge).unwrap();
    let button_pin_driver = Box::new(RefCell::new(button_driver));
    let _res = BUTTON_PIN.init(Box::leak(button_pin_driver));
    defmt::println!("Done setting up BUTTON Pin");

    // Initialize the pubsub channel for button pushes
    {
        defmt::println!("Setting up BUTTON PubSub Channel");
        let button_channel = Box::new(RefCell::new(PubSubChannel::<CriticalSectionRawMutex, u32, 1, 2, 1>::new()));
        let _res = BUTTON_CHANNEL.init(Box::leak(button_channel));
        defmt::println!("Done setting up BUTTON PubSub Channel");
    }

    // Initialize the pubsub channel for button pushes
    {
        defmt::println!("Setting up BUTTON Queue");
        let queue = Box::new(RefCell::new(Queue::new(10)));
        let _res = BUTTON_QUEUE.init(Box::leak(queue));
        defmt::println!("Done setting up BUTTON Queue");
    }

    // Run the asynchronous main function
    spawner.spawn(blinky()).unwrap();
    spawner.spawn(async_main()).unwrap();
    spawner.spawn(button_task()).unwrap();
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
async fn button_task() {
    let button_pin = BUTTON_PIN.get().await;
    defmt::println!("Setting up button task on pin {:?}", button_pin.borrow().pin());

    unsafe {
        button_pin.borrow_mut().subscribe(move || {
            // Assert FLAG indicating a press button happened
            // let channel_future = BUTTON_CHANNEL.try_get().with_timeout(Duration::from_millis(1000));
            // defmt::println!("Button pressed");

            // Fetch the current time without using chrono
            let system_time = SystemTime::now();
            // Convert system_time to a timestamp in milliseconds
            let timestamp_ms = system_time.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_millis() as i64;
            if let Some(res) = BUTTON_QUEUE.try_get() {
                let x = res.try_borrow();
                if x.is_ok() {
                    defmt::println!("Button pressed");
                    x.unwrap().send_back(timestamp_ms, 0).unwrap();
                } else {
                    defmt::println!("Button presseda");
                    defmt::println!("{:?}",x.err());
                }
            }
            // defmt::println!("Button pressed - {:?}", timestamp_ms);
        }).unwrap();
        button_pin.borrow_mut().enable_interrupt().unwrap();
    }


    loop {
        defmt::println!("Looping on button task");
        let queue = BUTTON_QUEUE.get().await;
        if let Some(msg) = queue.borrow().recv_front(1000) {
            defmt::println!("Button pressed - {:?}", msg);
            button_pin.borrow_mut().enable_interrupt().unwrap();
        } else {
            defmt::println!("No signal found");
        }
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