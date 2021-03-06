use std::{
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use super::{friendly_denom, RenderTimeTracer};
use super::{InfoBubble, TOOLTIPS};
use crate::{to_badgateway, to_badreq, State};
use askama::Template;
use num_traits::ToPrimitive;
use serde::Serialize;
use themelio_nodeprot::ValClientSnapshot;

use async_trait::async_trait;
use themelio_structs::{Denom, NetID, PoolKey, MICRO_CONVERTER};

#[derive(Template)]
#[template(path = "pool.html", escape = "none")]
struct PoolTemplate {
    testnet: bool,
    friendly_denom: String,
    pool_key: PoolKey,
    last_item: PoolDataItem,
    tooltips: &'static TOOLTIPS,
    denom_tooltip: &'static InfoBubble,
}

// 2 million cached pooldataitems is 64 mb
// 1 item is 256 bits
#[derive(Serialize, Clone)]
pub struct PoolDataItem {
    date: u64,
    height: u64,
    price: f64,
    liquidity: f64,
    ergs_per_mel: f64,
}

#[async_trait]
pub trait AsPoolDataItem {
    async fn as_pool_data_item(&self, pool_key: PoolKey) -> tide::Result<Option<PoolDataItem>>;
    async fn get_older_pool_data_item(
        &self,
        pool_key: PoolKey,
        height: u64,
    ) -> tide::Result<Option<PoolDataItem>>;
}

#[async_trait]
impl AsPoolDataItem for ValClientSnapshot {
    // returns a PoolDataItem that assumes the snapshot represents the most recent block
    async fn as_pool_data_item(&self, pool_key: PoolKey) -> tide::Result<Option<PoolDataItem>> {
        let height = self.current_header().height.0;
        Ok(self.get_pool(pool_key).await?.map(|pool_info| {
            let price = pool_info.implied_price().to_f64().unwrap_or_default();
            let liquidity =
                (pool_info.lefts as f64 * pool_info.rights as f64).sqrt() / MICRO_CONVERTER as f64;
            PoolDataItem {
                date: PoolDataItem::block_time(0),
                height,
                price,
                liquidity,
                ergs_per_mel: themelio_stf::dosc_to_erg(height.into(), 10000) as f64 / 10000.0,
            }
        }))
    }
    async fn get_older_pool_data_item(
        &self,
        pool_key: PoolKey,
        height: u64,
    ) -> tide::Result<Option<PoolDataItem>> {
        let last_height = self.current_header().height.0;
        let snapshot = self.get_older(height.into()).await.map_err(to_badgateway)?;
        let item = snapshot.as_pool_data_item(pool_key).await?;
        Ok(item.map(|mut item| item.set_time(last_height - height).clone()))
    }
}

impl PoolDataItem {
    pub fn block_time(distance_from_now: u64) -> u64 {
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            / 30
            * 30;
        now_unix - distance_from_now * 30
    }
    pub fn set_time(&mut self, distance_from_now: u64) -> &mut Self {
        self.date = PoolDataItem::block_time(distance_from_now);
        self
    }
    pub fn block_height(&self) -> u64 {
        self.height
    }
}

#[tracing::instrument(skip(req))]
pub async fn get_poolpage(req: tide::Request<State>) -> tide::Result<tide::Response> {
    let _render = RenderTimeTracer::new("poolpage");
    let pool_key = {
        let denom = req.param("denom_left").map(|v| v.to_string())?;
        let left = Denom::from_str(&denom).map_err(to_badreq)?;
        let denom = req.param("denom_right").map(|v| v.to_string())?;
        let right = Denom::from_str(&denom).map_err(to_badreq)?;
        PoolKey { left, right }
    }
    .to_canonical()
    .unwrap();

    let friendly_denom = friendly_denom(pool_key.right);
    let snapshot = req
        .state()
        .val_client
        .snapshot()
        .await
        .map_err(to_badgateway)?;
    let last_day = snapshot.as_pool_data_item(pool_key).await?;

    let pool_template = PoolTemplate {
        testnet: req.state().val_client.netid() == NetID::Testnet,
        pool_key: pool_key,
        denom_tooltip: &TOOLTIPS[&friendly_denom],
        friendly_denom: friendly_denom,
        last_item: last_day.unwrap(),
        tooltips: &TOOLTIPS,
    };
    let mut body: tide::Response = pool_template.render().unwrap().into();
    body.set_content_type("text/html");
    body.insert_header("cache-control", "max-age=31536000");
    Ok(body)
}
