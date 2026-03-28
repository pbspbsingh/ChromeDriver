use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::target::CloseTargetParams;
use std::time::Duration;
use tokio::time;

pub trait PageFeatures {
    fn nap(&self) -> impl Future<Output = &Self> + Send;

    fn sleep(&self) -> impl Future<Output = &Self> + Send;

    fn close_me(self) -> impl Future<Output = ()> + Send;
}

impl PageFeatures for Page {
    async fn nap(&self) -> &Self {
        time::sleep(Duration::from_millis(rand::random_range(20..=200))).await;
        self
    }

    async fn sleep(&self) -> &Self {
        time::sleep(Duration::from_millis(rand::random_range(200..=2000))).await;
        self
    }

    async fn close_me(self) {
        let target_id = self.target_id().clone();
        self.execute(CloseTargetParams::new(target_id)).await.ok();
    }
}
