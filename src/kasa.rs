use crate::TZ_OFFSET;
use async_io::Async;
use chrono::{DateTime, FixedOffset, TimeZone};
use defmt::Format;
use serde::{Deserialize, Serialize};
use serde_json::from_slice;
use std::io::{Error, ErrorKind, Read, Write};
use std::net::TcpStream;
use tplink_shome_protocol::{decrypt, encrypt};

const KASA_REQUEST: &[u8] =
    r#"{"system":{"get_sysinfo":{}},"emeter":{"get_realtime":{}}}"#.as_bytes();

#[derive(Deserialize, Debug, Format)]
pub(crate) struct KasaResponse {
    pub system: KasaSystemResponse,
    #[serde(rename = "emeter")]
    pub power: KasaPowerResponse,
}

#[derive(Deserialize, Debug, Format)]
pub(crate) struct KasaSystemResponse {
    pub get_sysinfo: KasaSysinfoResponse,
}

#[derive(Deserialize, Debug)]
pub(crate) struct KasaSysinfoResponse {
    pub alias: String,
    #[serde(rename = "deviceId")]
    pub device_id: String,
    pub model: Option<String>,
    pub sw_ver: Option<String>,
    pub hw_ver: Option<String>,
}

impl Format for KasaSysinfoResponse {
    fn format(&self, f: defmt::Formatter) {
        defmt::write!(f, "alias: {:a}", self.alias.as_str());
        defmt::write!(f, "deviceId: {:a}", self.device_id.as_str());
        defmt::write!(f, "model: {:a}", self.model.as_deref().unwrap_or("unknown"));
        defmt::write!(
            f,
            "sw_ver: {:a}",
            self.sw_ver.as_deref().unwrap_or("unknown")
        );
        defmt::write!(
            f,
            "hw_ver: {:a}",
            self.hw_ver.as_deref().unwrap_or("unknown")
        );
    }
}

#[derive(Deserialize, Debug, Format)]
pub(crate) struct KasaPowerResponse {
    pub get_realtime: Option<KasaRealtimeResponse>,
}

#[derive(Deserialize, Debug, Format)]
pub(crate) struct KasaRealtimeResponse {
    // v1 hardware returns f64 values in base units
    pub current: Option<f64>,
    pub voltage: Option<f64>,
    pub power: Option<f64>,
    pub total: Option<f64>,

    // v2 hardware returns u64 values in named units
    pub voltage_mv: Option<u64>,
    pub current_ma: Option<u64>,
    pub power_mw: Option<u64>,
    pub total_wh: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct KasaPowerDetails<Tz: TimeZone> {
    pub alias: String,
    pub device_id: String,
    pub voltage_mv: Option<u64>,
    pub current_ma: Option<u64>,
    pub power_mw: Option<u64>,
    pub total_wh: Option<u64>,
    pub timestamp: DateTime<Tz>,
}

impl<Tz: TimeZone> KasaPowerDetails<Tz> {
    // Compare this value to another value on all fields except the timestamp
    pub fn compare_no_ts(&self, other: &KasaPowerDetails<Tz>) -> bool {
        self.alias == other.alias
            && self.device_id == other.device_id
            && self.voltage_mv == other.voltage_mv
            && self.current_ma == other.current_ma
            && self.power_mw == other.power_mw
            && self.total_wh == other.total_wh
    }
}

impl From<KasaResponse> for KasaPowerDetails<FixedOffset> {
    fn from(response: KasaResponse) -> Self {
        let system = response.system.get_sysinfo;
        let power = response.power.get_realtime.unwrap();

        KasaPowerDetails {
            alias: system.alias,
            device_id: system.device_id,
            voltage_mv: power
                .voltage_mv
                .or_else(|| power.voltage.map(|v| (v * 1000.0) as u64)),
            current_ma: power
                .current_ma
                .or_else(|| power.current.map(|c| (c * 1000.0) as u64)),
            power_mw: power
                .power_mw
                .or_else(|| power.power.map(|p| (p * 1000.0) as u64)),
            total_wh: power
                .total_wh
                .or_else(|| power.total.map(|t| (t / 3600.0) as u64)),
            timestamp: chrono::Utc::now().with_timezone(TZ_OFFSET.get()),
        }
    }
}

impl<Tz: TimeZone> Format for KasaPowerDetails<Tz> {
    fn format(&self, f: defmt::Formatter) {
        defmt::write!(f, "alias: {:a} ", self.alias.as_str());
        defmt::write!(f, "deviceId: {:a} ", self.device_id.as_str());
        defmt::write!(f, "voltage: {=u64} mv ", self.voltage_mv.unwrap_or(0));
        defmt::write!(f, "current: {=u64} ma ", self.current_ma.unwrap_or(0));
        defmt::write!(f, "power: {=u64} mw ", self.power_mw.unwrap_or(0));
        defmt::write!(f, "total: {=u64} wh", self.total_wh.unwrap_or(0));
    }
}

pub(crate) async fn send_kasa_message<Tz: TimeZone>(
    stream: &mut Async<TcpStream>,
) -> Result<KasaPowerDetails<Tz>, Error>
where
    KasaPowerDetails<Tz>: From<KasaResponse>,
{
    let buf = encrypt(KASA_REQUEST);
    let len = &(buf.len() as u32).to_be_bytes();

    defmt::trace!("Sending Kasa request");
    unsafe {
        stream.write_with_mut(|w| w.write_all(len)).await?;
        stream.write_with_mut(|w| w.write_all(&buf)).await?;
    }

    defmt::trace!("Receiving Kasa request part 1");
    let mut buf = [0; 4];
    unsafe {
        stream.read_with_mut(|r| r.read_exact(&mut buf)).await?;
    }
    defmt::trace!("Receiving Kasa request part 2");

    let mut buf: Vec<u8> = vec![0; u32::from_be_bytes(buf) as usize];
    unsafe {
        stream.read_with_mut(|r| r.read_exact(&mut buf)).await?;
    }

    let x = &decrypt(&buf);
    defmt::trace!(
        "Processing kasa response from JSON: {:?}",
        x.clone().as_ascii().unwrap().as_str()
    );

    let kasa_response: KasaResponse =
        from_slice(&decrypt(&buf)).map_err(|_| Error::from(ErrorKind::InvalidData))?;

    Ok(kasa_response.into())
}
