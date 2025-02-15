#![feature(ascii_char)]

mod kasa;
mod net;

use crate::kasa::KasaPowerDetails;
use crate::net::WifiLoop;
use async_io::Async;
use async_mutex::Mutex;
use chrono::{DateTime, FixedOffset, TimeDelta, TimeZone, Utc};
use defmt::Format;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::lazy_lock::LazyLock;
use embassy_sync::once_lock::OnceLock;
use embassy_sync::pubsub::PubSubChannel;
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_idf_hal::gpio::{AnyIOPin, IOPin, Input, InterruptType, Output, Pin, PinDriver, Pull};
use esp_idf_hal::modem::Modem;
use esp_idf_hal::prelude::*;
use esp_idf_hal::sys::const_format::formatcp;
use esp_idf_svc::eventloop::{EspEventLoop, EspSystemEventLoop, System};
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::mqtt::client::{
    EspAsyncMqttClient, EspAsyncMqttConnection, EventPayload, MqttClientConfiguration, QoS,
};
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvsPartition, NvsDefault};
use esp_idf_svc::sntp;
use esp_idf_svc::sntp::{EspSntp, SntpConf};
use esp_idf_svc::sys::{link_patches, EspError};
use esp_idf_svc::timer::{EspTaskTimerService, EspTimerService, Task};
use esp_idf_svc::wifi::{AsyncWifi, EspWifi};
use esp_println as _;
use serde::ser::SerializeStruct;
use serde::Serialize;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;

static BUTTON_PIN: OnceLock<&mut RefCell<PinDriver<AnyIOPin, Input>>> = OnceLock::new();
static LED_PIN: OnceLock<&mut RefCell<PinDriver<AnyIOPin, Output>>> = OnceLock::new();
static BUTTON_CHANNEL: PubSubChannel<CriticalSectionRawMutex, i64, 1, 2, 1> = PubSubChannel::new();
static RESTART_LED_SEQUENCE: AtomicBool = AtomicBool::new(false);
static WIFI_LOOP: OnceLock<Mutex<WifiLoop>> = OnceLock::new();
static SNTP: OnceLock<&mut RefCell<EspSntp>> = OnceLock::new();
static MQTT_CLIENT: OnceLock<&mut RefCell<EspAsyncMqttClient>> = OnceLock::new();
static MQTT_CONN: OnceLock<&mut RefCell<EspAsyncMqttConnection>> = OnceLock::new();

const MQTT_PROTOCOL: &str = env!("MQTT_PROTOCOL");
const MQTT_URL: &str = env!("MQTT_SERVER");
const MQTT_PORT: &str = env!("MQTT_PORT");
const MQTT_USERNAME: &str = env!("MQTT_USERNAME");
const MQTT_PASSWORD: &str = env!("MQTT_PASSWORD");
const MQTT_CLIENT_ID: &str = env!("MQTT_CLIENT_ID");
const MQTT_TOPIC: &str = env!("MQTT_TOPIC");
static TZ_OFFSET: LazyLock<FixedOffset> = LazyLock::new(|| fixed_tz_offset(env!("TZ_OFFSET")));
fn fixed_tz_offset(hours: &str) -> FixedOffset {
    let hours = hours.parse::<i32>().unwrap();
    FixedOffset::west_opt(3600 * hours).unwrap()
}

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

async fn setup_wifi_loop(
    modem: Modem,
    sys_loop: EspEventLoop<System>,
    nvs: EspNvsPartition<NvsDefault>,
    timer_service: EspTimerService<Task>,
) -> Result<(), ()> {
    // Initialize the shared LED pin
    defmt::debug!("Setting up Wifi");

    let wifi: EspWifi = EspWifi::new(modem, sys_loop.clone(), Some(nvs)).unwrap();
    let async_wifi: AsyncWifi<_> = AsyncWifi::wrap(wifi, sys_loop.clone(), timer_service).unwrap();

    let wifi_loop = Mutex::new(WifiLoop::new(async_wifi));

    let _res = WIFI_LOOP.init(wifi_loop);
    defmt::debug!("Done setting up Wifi");
    Ok(())
}

async fn setup_sntp<F>(cb: F) -> Result<(), ()>
where
    F: FnMut(std::time::Duration) + Send + 'static,
{
    // Initialize the shared LED pin
    defmt::debug!("Setting up SNTP");
    let sntp_conf = SntpConf {
        ..Default::default()
    };

    let sntp = unsafe {
        Box::new(RefCell::new(
            sntp::EspSntp::new_nonstatic_with_callback(&sntp_conf, cb).unwrap(),
        ))
    };
    let _res = SNTP.init(Box::leak(sntp));
    defmt::debug!("Done setting up SNTP");
    Ok(())
}

fn sntp_cb(duration: std::time::Duration) {
    // The duration "looks like" a seconds since the epoch timestamp, so lets convert and see
    let tz = chrono::FixedOffset::west_opt(3600 * 5).unwrap();
    let utc_tz = DateTime::from_timestamp(duration.as_secs() as i64, 0).unwrap();
    let timestamp = tz.from_utc_datetime(&utc_tz.naive_utc());
    defmt::info!(
        "SNTP callback called with duration {:?}",
        timestamp.to_string().as_str()
    );
}

async fn setup_mqtt() -> Result<(), EspError> {
    // let mqtt = MqttClient::new(MQTT_URL, MQTT_PORT, MQTT_USERNAME, MQTT_PASSWORD, MQTT_CLIENT_ID);
    // mqtt.connect().unwrap();
    // mqtt.subscribe(MQTT_TOPIC).unwrap();
    const URL: &str = formatcp!("{MQTT_PROTOCOL}://{MQTT_URL}:{MQTT_PORT}");
    let (mqtt_client, mqtt_conn) = EspAsyncMqttClient::new(
        URL,
        &MqttClientConfiguration {
            client_id: Some(MQTT_CLIENT_ID),
            username: Some(MQTT_USERNAME),
            password: Some(MQTT_PASSWORD),
            ..Default::default()
        },
    )?;

    let mqtt_client = Box::new(RefCell::new(mqtt_client));
    let mqtt_conn = Box::new(RefCell::new(mqtt_conn));
    let _res = MQTT_CLIENT.init(Box::leak(mqtt_client));
    let _res = MQTT_CONN.init(Box::leak(mqtt_conn));

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
    setup_wifi_loop(peripherals.modem, sys_loop.clone(), nvs, timer_service)
        .await
        .unwrap();

    {
        let mut wifi_loop = WIFI_LOOP.get().await.lock().await;
        wifi_loop.configure().await.unwrap();
        defmt::info!("Waiting for Wifi to connect");
        wifi_loop.initial_connect().await.unwrap();
        defmt::info!("Wifi connected, continuing");
    }

    let kasa_device_addr = env!("KASA_DEVICE_ADDR")
        .to_socket_addrs()
        .unwrap()
        .next()
        .unwrap();

    // Run the asynchronous main function
    spawner.spawn(blinky()).unwrap();
    // spawner.spawn(async_main()).unwrap();
    spawner.spawn(button_task()).unwrap();
    spawner
        .spawn(net::wifi_loop(WIFI_LOOP.get().await))
        .unwrap();
    setup_sntp(sntp_cb).await.unwrap();
    setup_mqtt().await.unwrap();
    spawner.spawn(mqtt_event_loop()).unwrap();
    spawner.spawn(read_kasa_dev(kasa_device_addr)).unwrap();
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
            defmt::trace!("Looping on LED blinky task");
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

#[embassy_executor::task]
#[allow(clippy::await_holding_refcell_ref)]
async fn mqtt_event_loop() {
    // let client = MQTT_CLIENT.get().await;
    let conn = MQTT_CONN.get().await;

    loop {
        defmt::debug!("Waiting for mqtt event");
        let mut conn = conn.borrow_mut();
        let event = conn.next().await.unwrap();
        // defmt::debug!("Received mqtt event: {:?}", event.payload().to_string().as_str());
        match event.payload() {
            EventPayload::Published(publish) => {
                defmt::debug!("Received mqtt publish event: {:?}", publish);
            }
            EventPayload::Disconnected => {
                defmt::debug!("Received mqtt disconnected event");
            }
            EventPayload::Connected(status) => {
                defmt::debug!("Received mqtt connected event {:?}", status);
            }
            _ => {
                defmt::debug!(
                    "Received mqtt event: {:?}",
                    event.payload().to_string().as_str()
                );
            }
        }
    }
}

// Structure that stores a variable number of KasaPowerDetails events and a duration that represents
// how old an event is before it gets evicted from the structure
pub(crate) struct KasaPowerCache<Tz: TimeZone> {
    pub(crate) cache: VecDeque<KasaPowerDetails<Tz>>,
    pub(crate) duration: chrono::Duration,
}

impl<Tz: TimeZone> KasaPowerCache<Tz> {
    pub(crate) fn new(duration: chrono::Duration) -> Self {
        Self {
            cache: VecDeque::new(),
            duration,
        }
    }

    pub(crate) fn insert(&mut self, event: KasaPowerDetails<Tz>) {
        self.evict(event.timestamp.clone());
        self.cache.push_back(event);
    }

    pub(crate) fn evict(&mut self, since: DateTime<Tz>) {
        let threshold = since - self.duration;
        while let Some(event) = self.cache.front() {
            if event.timestamp >= threshold {
                break;
            }
            self.cache.pop_front();
        }
    }

    // Compute the median voltage of the events contained in the cache
    pub(crate) fn median_and_mean_voltage(&self) -> Option<(u64, f64)> {
        let voltages: Vec<u64> = self.cache.iter().filter_map(|e| e.voltage_mv).collect();
        KasaPowerCache::<Tz>::median_and_mean_value(voltages)
    }

    // Compute the median voltage of the events contained in the cache
    pub(crate) fn median_and_mean_current(&self) -> Option<(u64, f64)> {
        let voltages: Vec<u64> = self.cache.iter().filter_map(|e| e.current_ma).collect();
        KasaPowerCache::<Tz>::median_and_mean_value(voltages)
    }

    pub(crate) fn median_and_mean_power(&self) -> Option<(u64, f64)> {
        let voltages: Vec<u64> = self.cache.iter().filter_map(|e| e.power_mw).collect();
        KasaPowerCache::<Tz>::median_and_mean_value(voltages)
    }

    fn median_and_mean_value(mut values: Vec<u64>) -> Option<(u64, f64)> {
        values.sort_unstable();
        let len = values.len();
        if len == 0 {
            return None;
        }
        // get the average value from values
        let mean = values.iter().sum::<u64>() as f64 / values.len() as f64;
        // get the median value from values
        let median = if len % 2 == 0 {
            (values[len / 2] + values[len / 2 - 1]) / 2
        } else {
            values[len / 2]
        };
        Some((median, mean))
    }
}

impl Format for KasaPowerCache<FixedOffset> {
    fn format(&self, f: defmt::Formatter) {
        defmt::write!(
            f,
            "alias: {:a} ",
            self.cache.front().map(|x| x.alias.as_str())
        );
        defmt::write!(
            f,
            "deviceId: {:a} ",
            self.cache.front().map(|x| x.device_id.as_str())
        );
        let (median_voltage, mean_voltage) = self.median_and_mean_voltage().unwrap_or((0, 0.0));
        defmt::write!(f, "voltage (median): {=u64} mv ", median_voltage);
        defmt::write!(f, "voltage (mean): {=f64} mv ", mean_voltage);
        let (median_current, mean_current) = self.median_and_mean_current().unwrap_or((0, 0.0));
        defmt::write!(f, "current (median): {=u64} mv ", median_current);
        defmt::write!(f, "current (mean): {=f64} mv ", mean_current);
        let (median_power, mean_power) = self.median_and_mean_power().unwrap_or((0, 0.0));
        defmt::write!(f, "power (median): {=u64} mv ", median_power);
        defmt::write!(f, "power (mean): {=f64} mv ", mean_power);
        defmt::write!(
            f,
            "total: {=u64} wh",
            self.cache
                .back()
                .map(|x| x.total_wh.unwrap_or(0))
                .unwrap_or(0)
        );
    }
}

impl Serialize for KasaPowerCache<FixedOffset> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        let voltages_mv = &self
            .cache
            .iter()
            .map(|x| x.voltage_mv.unwrap_or(0))
            .collect::<Vec<u64>>();
        let currents_ma = &self
            .cache
            .iter()
            .map(|x| x.current_ma.unwrap_or(0))
            .collect::<Vec<u64>>();
        let powers_mw = &self
            .cache
            .iter()
            .map(|x| x.power_mw.unwrap_or(0))
            .collect::<Vec<u64>>();
        let timestamps = &self
            .cache
            .iter()
            .map(|x| x.timestamp.timestamp())
            .collect::<Vec<i64>>();
        let total_wh = self
            .cache
            .back()
            .map(|x| x.total_wh.unwrap_or(0))
            .unwrap_or(0);
        let num_readings = self.cache.len();
        let mut state = serializer.serialize_struct("KasaPowerCache", 8)?;
        state.serialize_field("alias", &self.cache.front().map(|x| x.alias.as_str()))?;
        state.serialize_field(
            "deviceId",
            &self.cache.front().map(|x| x.device_id.as_str()),
        )?;
        state.serialize_field("power_total", &total_wh)?;
        state.serialize_field("voltages_mv", &voltages_mv)?;
        state.serialize_field("currents_ma", &currents_ma)?;
        state.serialize_field("powers_mw", &powers_mw)?;
        state.serialize_field("timestamps", &timestamps)?;
        state.serialize_field("num_readings", &num_readings)?;
        state.end()
    }
}

#[embassy_executor::task]
#[allow(clippy::await_holding_refcell_ref)]
async fn read_kasa_dev(addr: SocketAddr) {
    let _mounted_eventfs = esp_idf_svc::io::vfs::MountedEventfs::mount(5).unwrap();
    let mut last_response: Option<KasaPowerDetails<FixedOffset>> = Option::None;
    let mut cache = KasaPowerCache::new(chrono::Duration::seconds(20));
    let publish_interval = chrono::Duration::seconds(15);
    let mut next_publish = chrono::Utc::now();

    loop {
        defmt::info!("Connecting to Kasa Device...");

        let mut stream = match Async::<TcpStream>::connect(addr).await {
            Ok(stream) => stream,
            Err(e) => {
                defmt::error!(
                    "Error connecting to Kasa Device: {:?}",
                    e.to_string().as_str()
                );
                Timer::after(Duration::from_secs(5)).await;
                continue;
            }
        };

        defmt::info!("Sending request to Kasa device...");
        let result: KasaPowerDetails<FixedOffset> = match kasa::send_kasa_message(&mut stream).await
        {
            Ok(response) => response,
            Err(e) => {
                defmt::error!("Error talking to kasa device: {:?}", e.to_string().as_str());
                Timer::after(Duration::from_secs(5)).await;
                continue;
            }
        };

        match last_response.as_mut() {
            Some(last) => {
                if !last.compare_no_ts(&result) {
                    defmt::debug!("Power state changed: {:?}", result);
                    *last = result.clone();
                    cache.insert(result);
                    if chrono::Utc::now() > next_publish {
                        let client = MQTT_CLIENT.get().await;
                        let message: String = serde_json::to_string(&cache).unwrap();
                        let mut client = client.borrow_mut();
                        defmt::debug!(
                            "Sending new power state changed to mqtt: {:?}",
                            message.as_str()
                        );

                        let result = client
                            .publish(MQTT_TOPIC, QoS::AtMostOnce, false, message.as_bytes())
                            .await
                            .unwrap();
                        defmt::debug!(
                            "Published message: {:?} messageId:{:?}",
                            message.as_str(),
                            result
                        );
                        next_publish = chrono::Utc::now() + publish_interval;
                    }
                } else {
                    defmt::debug!("No change in power state");
                }
            }
            None => {
                defmt::debug!("Initial power state: {:?}", result);
                last_response = Some(result);
            }
        }

        Timer::after(Duration::from_millis(500)).await;
    }
}
