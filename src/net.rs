use embassy_sync::once_lock::OnceLock;
use embassy_time::{Duration, Timer};
use esp_idf_svc::wifi::{
    AsyncWifi, AuthMethod, ClientConfiguration, Configuration, EspWifi, PmfConfiguration,
    ScanMethod,
};
use std::cell::RefCell;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;

const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASSWORD");

#[embassy_executor::task]
#[allow(clippy::await_holding_refcell_ref)]
pub async fn connect_wifi(
    wifi: &'static OnceLock<&'static mut RefCell<AsyncWifi<EspWifi<'static>>>>,
    connected: &'static AtomicBool,
) {
    loop {
        if let Ok(mut wifi) = wifi.get().await.try_borrow_mut() {
            if !wifi.is_started().unwrap_or(false) {
                let wifi_configuration: Configuration =
                    Configuration::Client(ClientConfiguration {
                        ssid: SSID.try_into().unwrap(),
                        bssid: None,
                        auth_method: AuthMethod::WPA2Personal,
                        password: PASSWORD.try_into().unwrap(),
                        scan_method: ScanMethod::FastScan,
                        pmf_cfg: PmfConfiguration::Capable { required: false },
                        ..Default::default()
                    });

                wifi.set_configuration(&wifi_configuration)
                    .expect("Error setting wifi configuration");

                defmt::info!("Starting wifi");
                wifi.start()
                    .await
                    .expect("Failed while waiting for wifi to start");
                defmt::info!("Wifi started");
            }

            if !wifi.is_connected().unwrap_or(false) {
                connected.store(false, Relaxed);
                defmt::info!("Connecting to wifi");
                if let Some(err) = wifi.connect().await.err() {
                    defmt::error!("Failed to connect to wifi: {:?}", err.to_string().as_str());
                    continue;
                }
                defmt::info!("Wifi connected");

                if let Some(err) = wifi.wait_netif_up().await.err() {
                    defmt::error!(
                        "Failed to wait for wifi netif up: {:?}",
                        err.to_string().as_str()
                    );
                    // if let Ok(res) = wifi.scan().await {
                    //     res.iter().for_each(|ap| {
                    //         defmt::info!("AP: {:?}", ap.ssid);
                    //     });
                    // }
                    // wifi.stop().await.unwrap();
                    continue;
                };
                defmt::info!("Wifi netif up");
                connected.store(true, Relaxed);
            }
        }

        Timer::after(Duration::from_millis(100)).await;
    }
}
