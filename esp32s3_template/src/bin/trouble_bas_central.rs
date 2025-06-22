#![no_std]
#![no_main]

use embassy_futures::join::join;
use embassy_time::{Duration, Timer};
use trouble_host::prelude::*;
use embassy_executor::Spawner;
use esp_hal::{clock::CpuClock, timer::timg::TimerGroup};
use esp_wifi::ble::controller::BleConnector;
use log::{info, warn};
use {esp_alloc as _, esp_backtrace as _};


/// Max number of connections
const CONNECTIONS_MAX: usize = 1;

/// Max number of L2CAP channels.
const L2CAP_CHANNELS_MAX: usize = 3; // Signal + att + CoC


#[esp_hal_embassy::main]
async fn main(_s: Spawner) {
    esp_println::logger::init_logger_from_env();
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    esp_alloc::heap_allocator!(size: 72 * 1024);
    let timg0 = TimerGroup::new(peripherals.TIMG0);

    let init = esp_wifi::init(
        timg0.timer0,
        esp_hal::rng::Rng::new(peripherals.RNG),
        peripherals.RADIO_CLK,
    )
    .unwrap();

    #[cfg(not(feature = "esp32"))]
    {
        let systimer = esp_hal::timer::systimer::SystemTimer::new(peripherals.SYSTIMER);
        esp_hal_embassy::init(systimer.alarm0);
    }
    #[cfg(feature = "esp32")]
    {
        esp_hal_embassy::init(timg0.timer1);
    }

    let bluetooth = peripherals.BT;
    let connector = BleConnector::new(&init, bluetooth);
    let controller: ExternalController<_, 20> = ExternalController::new(connector);

    run(controller).await;
}


pub async fn run<C>(controller: C)
where
    C: Controller,
{
    // Using a fixed "random" address can be useful for testing. In real scenarios, one would
    // use e.g. the MAC 6 byte array as the address (how to get that varies by the platform).
    let address: Address = Address::random([0xff, 0x8f, 0x1b, 0x05, 0xe4, 0xff]);
    info!("Our address = {:?}", address);

    let mut resources: HostResources<DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX> = HostResources::new();
    let stack = trouble_host::new(controller, &mut resources).set_random_address(address);
    let Host {
        mut central,
        mut runner,
        ..
    } = stack.build();

    // NOTE: Modify this to match the address of the peripheral you want to connect to.
    // Currently it matches the address used by the peripheral examples
    let target: Address = Address::random([0xff, 0x8f, 0x1a, 0x05, 0xe4, 0xff]);

    let config = ConnectConfig {
        connect_params: Default::default(),
        scan_config: ScanConfig {
            filter_accept_list: &[(target.kind, &target.addr)],
            ..Default::default()
        },
    };

    info!("Scanning for peripheral...");
    let _ = join(runner.run(), async {
        info!("Connecting");

        let conn = central.connect(&config).await.unwrap();
        info!("Connected, creating gatt client");

        let client = GattClient::<C, DefaultPacketPool, 10>::new(&stack, &conn)
            .await
            .unwrap();

        let _ = join(client.task(), async {
            info!("Looking for battery service");
            let services = client.services_by_uuid(&Uuid::new_short(0x180f)).await.unwrap();
            let service = services.first().unwrap().clone();

            info!("Looking for value handle");
            let c: Characteristic<u8> = client
                .characteristic_by_uuid(&service, &Uuid::new_short(0x2a19))
                .await
                .unwrap();

            info!("Subscribing notifications");
            let mut listener = client.subscribe(&c, false).await.unwrap();

            let _ = join(
                async {
                    loop {
                        let mut data = [0; 1];
                        client.read_characteristic(&c, &mut data[..]).await.unwrap();
                        info!("Read value: {}", data[0]);
                        Timer::after(Duration::from_secs(10)).await;
                    }
                },
                async {
                    loop {
                        let data = listener.next().await;
                        info!("Got notification: {:?} (val: {})", data.as_ref(), data.as_ref()[0]);
                    }
                },
            )
            .await;
        })
        .await;
    })
    .await;
}