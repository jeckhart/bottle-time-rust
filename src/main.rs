
mod net;

use chrono::{DateTime, TimeDelta, Utc};
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::once_lock::OnceLock;
use embassy_sync::pubsub::PubSubChannel;
use embassy_time::{Duration, Timer};
use esp_idf_hal::prelude::*;
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::sys::link_patches;
use std::cell::RefCell;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;

use esp_backtrace as _;
use esp_idf_hal::gpio::{AnyIOPin, IOPin, Input, InterruptType, Output, Pin, PinDriver, Pull};
use esp_idf_hal::modem::Modem;
use esp_idf_svc::eventloop::{EspEventLoop, EspSystemEventLoop, System};
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvsPartition, NvsDefault};
use esp_idf_svc::timer::{EspTaskTimerService, EspTimerService, Task};
use esp_idf_svc::wifi::{AsyncWifi, EspWifi};
use esp_println as _;

static BUTTON_PIN: OnceLock<&mut RefCell<PinDriver<AnyIOPin, Input>>> = OnceLock::new();
static LED_PIN: OnceLock<&mut RefCell<PinDriver<AnyIOPin, Output>>> = OnceLock::new();
static BUTTON_CHANNEL: PubSubChannel<CriticalSectionRawMutex, i64, 1, 2, 1> = PubSubChannel::new();
static RESTART_LED_SEQUENCE: AtomicBool = AtomicBool::new(false);
static WIFI_CONNECTED: AtomicBool = AtomicBool::new(false);
static WIFI: OnceLock<&mut RefCell<AsyncWifi<EspWifi>>> = OnceLock::new();

async fn setup_led_pin(
    led_pin: AnyIOPin,
    pin_lock: &OnceLock<&mut RefCell<PinDriver<'_, AnyIOPin, Output>>>,
) -> Result<(), ()> {
    // Initialize the shared LED pin
    defmt::debug!("Setting up LED Pin on pin {:?}", led_pin.pin());
    let led_pin_driver = Box::new(RefCell::new(
        PinDriver::output(led_pin.downgrade()).unwrap(),
    ));
    let _res = pin_lock.init(Box::leak(led_pin_driver));
    defmt::debug!("Done setting up LED Pin");
    Ok(())
}

async fn setup_button_pin(
    button_pin: AnyIOPin,
    pin_lock: &OnceLock<&mut RefCell<PinDriver<'_, AnyIOPin, Input>>>,
) -> Result<(), ()> {
    // Initialize the shared LED pin
    defmt::println!("Setting up BUTTON Pin on pin {:?}", button_pin.pin());
    let mut button_driver = PinDriver::input(button_pin.downgrade()).unwrap();
    button_driver.set_pull(Pull::Up).unwrap();
    button_driver
        .set_interrupt_type(InterruptType::AnyEdge)
        .unwrap();
    let button_pin_driver = Box::new(RefCell::new(button_driver));
    let _res = pin_lock.init(Box::leak(button_pin_driver));
    defmt::println!("Done setting up BUTTON Pin");
    Ok(())
}

async fn setup_wifi(
    modem: Modem,
    sys_loop: EspEventLoop<System>,
    nvs: EspNvsPartition<NvsDefault>,
    timer_service: EspTimerService<Task>,
) -> Result<(), ()> {
    // Initialize the shared LED pin
    defmt::debug!("Setting up Wifi");

    let wifi = Box::new(RefCell::new(
        AsyncWifi::wrap(
            EspWifi::new(modem, sys_loop.clone(), Some(nvs)).unwrap(),
            sys_loop.clone(),
            timer_service,
        )
        .unwrap(),
    ));

    let _res = WIFI.init(Box::leak(wifi));
    defmt::debug!("Done setting up Wifi");
    Ok(())
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // Necessary for linking patches to the runtime
    link_patches();

    // Initialize the logger
    EspLogger::initialize_default();

    let current_time = chrono::Local::now();
    let datetime: DateTime<Utc> = current_time.into();
    defmt::println!(
        "Starting async main - current time {:?}",
        datetime.to_rfc3339().as_str()
    );

    let peripherals = Peripherals::take().unwrap();
    let sys_loop = EspSystemEventLoop::take().unwrap();
    let timer_service = EspTaskTimerService::new().unwrap();
    let nvs = EspDefaultNvsPartition::take().unwrap();

    // Initialize the shared pins
    setup_led_pin(peripherals.pins.gpio2.downgrade(), &LED_PIN)
        .await
        .unwrap();
    setup_button_pin(peripherals.pins.gpio4.downgrade(), &BUTTON_PIN)
        .await
        .unwrap();
    setup_wifi(peripherals.modem, sys_loop.clone(), nvs, timer_service)
        .await
        .unwrap();

    // Run the asynchronous main function
    spawner.spawn(blinky()).unwrap();
    spawner.spawn(async_main()).unwrap();
    spawner.spawn(button_task()).unwrap();
    spawner
        .spawn(net::connect_wifi(&WIFI, &WIFI_CONNECTED))
        .unwrap();
}

async fn wait_for_duration_with_interrupt(
    duration: Duration,
    interrupt: &AtomicBool,
    poll_time: Option<Duration>,
) -> bool {
    let poll_time = poll_time.unwrap_or(Duration::from_millis(100));

    let start_time = chrono::Local::now();
    // Convert Duration to TimeDelta
    let td = TimeDelta::milliseconds(duration.as_millis() as i64);
    while chrono::Local::now() - start_time < td && !interrupt.load(Relaxed) {
        Timer::after(poll_time).await;
    }
    !interrupt.load(Relaxed)
}

#[embassy_executor::task]
async fn blinky() {
    let led_pin = LED_PIN.get().await;
    defmt::println!(
        "Setting up LED blinky task on pin {:?}",
        led_pin.borrow().pin()
    );

    let mut subscriber = BUTTON_CHANNEL.dyn_subscriber().unwrap();

    loop {
        RESTART_LED_SEQUENCE.store(false, Relaxed);
        if let Some(msg) = subscriber.try_next_message() {
            defmt::info!(
                "Signal to LED task from BUTTON task found {:?}",
                msg.clone()
            );

            // For loop that runs for 5 seconds and blinks the led every 100 ms
            let start_time = chrono::Local::now();
            while chrono::Local::now() - start_time < TimeDelta::seconds(5)
                && !RESTART_LED_SEQUENCE.load(Relaxed)
            {
                if led_pin.borrow_mut().is_set_high() {
                    led_pin.borrow_mut().set_low().unwrap();
                } else {
                    led_pin.borrow_mut().set_high().unwrap();
                }
                Timer::after(Duration::from_millis(100)).await;
            }
            defmt::info!("Done signalling LED from BUTTON");
        } else {
            defmt::println!("Looping on LED blinky task");
            led_pin.borrow_mut().set_high().unwrap();
            if !wait_for_duration_with_interrupt(
                Duration::from_secs(1),
                &RESTART_LED_SEQUENCE,
                Some(Duration::from_millis(100)),
            )
            .await
            {
                continue;
            }
            led_pin.borrow_mut().set_low().unwrap();
            if !wait_for_duration_with_interrupt(
                Duration::from_secs(1),
                &RESTART_LED_SEQUENCE,
                Some(Duration::from_millis(100)),
            )
            .await
            {
                continue;
            }
        }
    }
}

#[embassy_executor::task]
#[allow(clippy::await_holding_refcell_ref)]
async fn button_task() {
    let button_pin = BUTTON_PIN.get().await;
    defmt::println!(
        "Setting up button task on pin {:?}",
        button_pin.borrow().pin()
    );
    let publisher = BUTTON_CHANNEL.publisher().unwrap();

    loop {
        defmt::println!("Looping on button task");

        // This is the only place we borrow the button pin, so this should be safe to ignore the clippy warning here.
        {
            let mut button = button_pin.borrow_mut();
            button.wait_for_rising_edge().await.unwrap();
        }

        let current_time = chrono::Local::now();
        defmt::println!("Button pressed at {:?}", current_time.to_rfc3339().as_str());
        publisher.publish_immediate(current_time.timestamp_micros());
        RESTART_LED_SEQUENCE.store(true, std::sync::atomic::Ordering::Relaxed);
        Timer::after(Duration::from_millis(1000)).await;
    }
}

#[embassy_executor::task]
async fn async_main() {
    defmt::println!("Starting async loop");
    loop {
        defmt::info!("Asynchronous task running...");
        Timer::after(Duration::from_secs(1)).await;
    }
}
