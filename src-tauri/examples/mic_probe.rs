use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait};

fn main() {
    let host = cpal::default_host();
    let Some(device) = host.default_input_device() else {
        println!("no default input device");
        return;
    };
    println!("device: {device}");

    match device.default_input_config() {
        Ok(cfg) => println!(
            "default cfg: {:?} {:?} {:?}",
            cfg.sample_format(),
            cfg.sample_rate(),
            cfg.channels()
        ),
        Err(e) => println!("default cfg err: {e}"),
    }

    match device.supported_input_configs() {
        Ok(configs) => {
            for c in configs {
                println!(
                    "supported: {:?} {} ch, rates {:?}-{:?}",
                    c.sample_format(),
                    c.channels(),
                    c.min_sample_rate(),
                    c.max_sample_rate()
                );
            }
        }
        Err(e) => println!("supported err: {e}"),
    }

    // 尝试直接以 16k mono f32 打开输入流
    let err = device.build_input_stream(
        cpal::StreamConfig {
            channels: 1,
            sample_rate: 16000,
            buffer_size: cpal::BufferSize::Default,
        },
        |data: &[f32], _| {},
        |e| eprintln!("stream err: {e}"),
        None,
    );
    match err {
        Ok(stream) => {
            println!("16k mono f32 stream OK");
            std::thread::sleep(Duration::from_millis(500));
        }
        Err(e) => println!("16k mono f32 stream FAILED: {e}"),
    }
}
