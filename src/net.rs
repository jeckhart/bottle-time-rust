use async_mutex::Mutex;
use embassy_time::{Duration, Timer};
use esp_idf_hal::sys::EspError;
use esp_idf_svc::wifi::{AsyncWifi, ClientConfiguration, Configuration, EspWifi, ScanMethod};
use esp_idf_sys as _;

const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASSWORD");

pub struct WifiLoop {
    wifi: AsyncWifi<EspWifi<'static>>,
}

impl WifiLoop {
    pub fn new(wifi: AsyncWifi<EspWifi<'static>>) -> Self {
        Self { wifi }
    }

    pub async fn configure(&mut self) -> Result<(), EspError> {
        let wifi_configuration: Configuration = Configuration::Client(ClientConfiguration {
            ssid: SSID.try_into().unwrap(),
            // bssid: None,
            // auth_method: AuthMethod::WPA2Personal,
            password: PASSWORD.try_into().unwrap(),
            scan_method: ScanMethod::FastScan,
            // pmf_cfg: PmfConfiguration::Capable { required: false },
            ..Default::default()
        });

        self.wifi.set_configuration(&wifi_configuration)?;

        defmt::info!("Starting Wi-Fi driver...");
        self.wifi.start().await
    }

    pub async fn initial_connect(&mut self) -> Result<(), EspError> {
        self.do_connect_loop(true).await
    }

    pub async fn stay_connected(&mut self) -> Result<(), EspError> {
        self.do_connect_loop(false).await
    }

    async fn do_connect_loop(&mut self, exit_after_first_connect: bool) -> Result<(), EspError> {
        let wifi = &mut self.wifi;

        loop {
            wifi.wifi_wait(|w| w.is_up(), None).await?;

            defmt::info!("Connecting to Wi-Fi...");
            wifi.connect().await?;

            defmt::info!("Waiting for association...");
            wifi.ip_wait_while(|w| w.is_up().map(|s| !s), None).await?;

            if exit_after_first_connect {
                return Ok(());
            }
        }
    }
}

#[embassy_executor::task]
pub async fn wifi_loop(wifi_loop: &'static Mutex<WifiLoop>) {
    defmt::info!("Starting Wifi Loop");

    loop {
        {
            let mut wl = wifi_loop.lock().await;
            let res = wl.stay_connected().await;
            if let Err(err) = res {
                defmt::error!("Error connecting to wifi: {:?}", err.to_string().as_str());
            }
        }
        Timer::after(Duration::from_millis(100)).await;
    }
}
