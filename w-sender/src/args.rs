use anyhow::{bail, Result};
use clap::Parser;
use lan_audio_common::audio::SAMPLE_RATE;
use std::net::SocketAddr;

#[derive(Clone, Debug, Parser)]
#[command(about = "Windows WASAPI event-driven LAN audio UDP sender")]
pub struct Args {
    #[arg(long, default_value = "127.0.0.1:50000")]
    pub target: SocketAddr,

    #[arg(long, default_value = "0.0.0.0:0")]
    pub bind: SocketAddr,

    #[arg(long, default_value = "CABLE Output")]
    pub device: String,

    #[arg(long)]
    pub list_devices: bool,

    #[arg(long, default_value_t = 240)]
    pub max_packet_frames: usize,

    #[arg(long)]
    pub require_48k_stereo: bool,

    #[arg(long)]
    pub duration_sec: Option<f64>,

    #[arg(long, default_value_t = 1.0)]
    pub metrics_interval_sec: f64,
}

impl Args {
    pub fn validate(&self) -> Result<()> {
        if self.max_packet_frames == 0 {
            bail!("--max-packet-frames must be greater than zero");
        }
        if self.max_packet_frames > u16::MAX as usize {
            bail!("--max-packet-frames must fit in the packet header");
        }
        if self.metrics_interval_sec <= 0.0 {
            bail!("--metrics-interval-sec must be greater than zero");
        }
        if self
            .duration_sec
            .map(|duration| duration <= 0.0)
            .unwrap_or(false)
        {
            bail!("--duration-sec must be greater than zero");
        }
        let _ = SAMPLE_RATE;
        Ok(())
    }
}
