use std::time::Duration;

use yash_app_events_capture::LatestFrameSlot;
use yash_app_events_capture_pw::{PortalCapture, PortalOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let slot = LatestFrameSlot::default();
    let capture = PortalCapture::start(PortalOptions::default(), slot.clone()).await?;
    println!("selected {}", capture.selected_source().label);
    let first = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(frame) = slot.latest() {
                break frame;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    println!(
        "frame {}: {}x{} stride={} format={:?}",
        first.sequence, first.width, first.height, first.row_stride, first.format
    );
    tokio::time::sleep(Duration::from_secs(1)).await;
    println!("metrics: {:?}", capture.metrics());
    capture.stop().await;
    println!("capture stopped");
    Ok(())
}
