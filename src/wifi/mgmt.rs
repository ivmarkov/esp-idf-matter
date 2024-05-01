#![cfg(all(
    not(esp32h2),
    esp_idf_comp_esp_wifi_enabled,
    esp_idf_comp_esp_event_enabled,
))]

use core::pin::pin;

use alloc::sync::Arc;

use embassy_futures::select::select;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Timer};

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::task::embassy_sync::EspRawMutex;
use esp_idf_svc::netif::EspNetif;
use esp_idf_svc::sys::{EspError, ESP_ERR_INVALID_STATE};
use esp_idf_svc::wifi::{self as wifi, AsyncWifi, AuthMethod, EspWifi, WifiEvent};

use log::{error, info, warn};

use rs_matter::data_model::sdm::nw_commissioning::NetworkCommissioningStatus;
use rs_matter::utils::notification::Notification;

use crate::netif::NetifAccess;

use super::{WifiContext, WifiCredentials, WifiStatus};

impl<'d, M> NetifAccess for &Mutex<M, AsyncWifi<&mut EspWifi<'d>>>
where
    M: RawMutex,
{
    async fn with_netif<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&EspNetif) -> R,
    {
        f(self.lock().await.wifi().sta_netif())
    }
}

pub struct WifiManager<'a, 'd, const N: usize, M>
where
    M: RawMutex,
{
    wifi: &'a Mutex<M, AsyncWifi<&'a mut EspWifi<'d>>>,
    context: &'a WifiContext<N, M>,
    sysloop: EspSystemEventLoop,
}

impl<'a, 'd, const N: usize, M> WifiManager<'a, 'd, N, M>
where
    M: RawMutex,
{
    pub fn new(
        wifi: &'a Mutex<M, AsyncWifi<&'a mut EspWifi<'d>>>,
        context: &'a WifiContext<N, M>,
        sysloop: EspSystemEventLoop,
    ) -> Self {
        Self {
            wifi,
            context,
            sysloop,
        }
    }

    pub async fn run(&self) -> Result<(), crate::error::Error> {
        let mut ssid = None;

        loop {
            let creds = self.context.state.lock(|state| {
                let mut state = state.borrow_mut();

                state.get_next_network(ssid.as_deref())
            });

            let Some(creds) = creds else {
                // No networks, bail out
                warn!("No networks found");
                return Err(EspError::from_infallible::<ESP_ERR_INVALID_STATE>().into());
            };

            ssid = Some(creds.ssid.clone());

            let _ = self.connect_with_retries(&creds).await;
        }
    }

    async fn connect_with_retries(&self, creds: &WifiCredentials) -> Result<(), EspError> {
        loop {
            let mut result = Ok(());

            for delay in [2, 5, 10, 20, 30, 60].iter().copied() {
                result = self.connect(creds).await;

                if result.is_ok() {
                    break;
                } else {
                    warn!(
                        "Connection to SSID {} failed: {:?}, retrying in {delay}s",
                        creds.ssid, result
                    );
                }

                Timer::after(Duration::from_secs(delay)).await;
            }

            self.context.state.lock(|state| {
                let mut state = state.borrow_mut();

                if result.is_ok() {
                    state.connected_once = true;
                }

                state.status = Some(WifiStatus {
                    ssid: creds.ssid.clone(),
                    status: result
                        .map(|_| NetworkCommissioningStatus::Success)
                        .unwrap_or(NetworkCommissioningStatus::OtherConnectionFailure),
                    value: 0,
                });
            });

            if result.is_ok() {
                info!("Connected to SSID {}", creds.ssid);
                self.wait_disconnect().await?;
            } else {
                error!("Failed to connect to SSID {}: {:?}", creds.ssid, result);
                break result;
            }
        }
    }

    async fn wait_disconnect(&self) -> Result<(), EspError> {
        let notification = Arc::new(Notification::<EspRawMutex>::new());

        let _subscription = {
            let notification = notification.clone();

            self.sysloop.subscribe::<WifiEvent, _>(move |_| {
                notification.notify();
            })
        }?;

        loop {
            {
                let wifi = self.wifi.lock().await;
                if !wifi.is_connected()? {
                    break Ok(());
                }
            }

            let mut events = pin!(notification.wait());
            let mut timer = pin!(Timer::after(Duration::from_secs(5)));

            select(&mut events, &mut timer).await;
        }
    }

    async fn connect(&self, creds: &WifiCredentials) -> Result<(), EspError> {
        info!("Connecting to SSID {}", creds.ssid);

        let auth_methods: &[AuthMethod] = if creds.password.is_empty() {
            &[AuthMethod::None]
        } else {
            &[
                AuthMethod::WPA2WPA3Personal,
                AuthMethod::WPAWPA2Personal,
                AuthMethod::WEP,
            ]
        };

        let mut result = Ok(());

        for auth_method in auth_methods.iter().copied() {
            let conf = wifi::Configuration::Client(wifi::ClientConfiguration {
                ssid: creds.ssid.clone(),
                auth_method,
                password: creds.password.clone(),
                ..Default::default()
            });

            result = self.connect_with(&conf).await;

            if result.is_ok() {
                break;
            }
        }

        result
    }

    async fn connect_with(&self, conf: &wifi::Configuration) -> Result<(), EspError> {
        info!("Connecting with {:?}", conf);

        let mut wifi = self.wifi.lock().await;

        let _ = wifi.stop().await;

        wifi.set_configuration(conf)?;
        info!("Configuration set");

        wifi.start().await?;

        info!("Wifi driver started");

        let connect = matches!(conf, wifi::Configuration::Client(_))
            && !matches!(
                conf,
                wifi::Configuration::Client(wifi::ClientConfiguration {
                    auth_method: wifi::AuthMethod::None,
                    ..
                })
            );

        if connect {
            info!("Connecting...");
            wifi.connect().await?;
        }

        info!("Successfully connected with {:?}", conf);

        Ok(())
    }
}
