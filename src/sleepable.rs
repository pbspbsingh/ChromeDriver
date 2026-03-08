use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::target::CloseTargetParams;
use std::time::Duration;

pub trait Sleepable {
    fn sleep(&self) -> impl std::future::Future<Output = &Self> + Send;

    fn close_me(&mut self) -> impl std::future::Future<Output = ()> + Send;
}

impl Sleepable for Page {
    async fn sleep(&self) -> &Self {
        tokio::time::sleep(Duration::from_millis(rand::random_range(200..2000))).await;
        self
    }

    async fn close_me(&mut self) {
        let target_id = self.target_id().clone();
        self.execute(CloseTargetParams::new(target_id)).await.ok();
    }
}
