use crate::to_badgateway;
use askama::Template;
use futures_util::stream::FuturesOrdered;
use futures_util::StreamExt;
use num_traits::{Inv, ToPrimitive};
use themelio_nodeprot::ValClient;
use themelio_stf::{CoinID, Denom, Header, NetID, PoolKey};
use tide::Body;
use super::{MicroUnit, RenderTimeTracer, InfoBubble};
use crate::utils::*;
#[derive(Template, serde::Serialize)]
#[template(path = "homepage.html")]
struct HomepageTemplate {
    testnet: bool,
    blocks: Vec<BlockSummary>,
    pool: PoolSummary,
    tooltips: ToolTips,
}


#[derive(serde::Serialize, serde::Deserialize)]
struct ToolTips {
    test: InfoBubble,
}
#[derive(serde::Serialize)]
// A block summary for the homepage.
pub struct BlockSummary {
    pub header: Header,
    pub total_weight: u128,
    pub reward_amount: MicroUnit,
    pub transactions: Vec<TransactionSummary>,
}


#[derive(serde::Serialize, Clone)]
// A transaction summary for the homepage.
pub struct TransactionSummary {
    pub hash: String,
    pub shorthash: String,
    pub height: u64,
    pub _weight: u128,
    pub mel_moved: MicroUnit,
}

#[derive(serde::Serialize)]
// A pool summary for the homepage.
struct PoolSummary {
    mel_per_sym: f64,
    mel_per_dosc: f64,
}

/// Homepage
#[tracing::instrument(skip(req))]
pub async fn get_homepage(req: tide::Request<ValClient>) -> tide::Result<Body> {
    let _render = RenderTimeTracer::new("homepage");

    let last_snap = req.state().snapshot().await.map_err(to_badgateway)?;
    let mut blocks = Vec::new();
    let mut futs = get_old_blocks(&last_snap, 30);

    while let Some(inner) = futs.next().await {
        let (block, reward) = inner.map_err(to_badgateway)?;
        let transactions: Vec<TransactionSummary> = get_transactions(&block, 30);

        blocks.push(BlockSummary { 
            header: block.header,
            total_weight: block.transactions.iter().map(|v| v.weight()).sum(),
            reward_amount: MicroUnit(reward.into(), "MEL".into()),
            transactions: transactions.clone(),
        });
    }

    let mel_per_dosc = (last_snap
        .get_pool(PoolKey::new(Denom::Mel, Denom::NomDosc))
        .await
        .map_err(to_badgateway)?
        .unwrap()
        .implied_price()
        .inv()
        * themelio_stf::dosc_inflator(last_snap.current_header().height))
    .to_f64()
    .unwrap_or_default();

    let pool = PoolSummary {
        mel_per_sym: last_snap
            .get_pool(PoolKey::new(Denom::Mel, Denom::Sym))
            .await
            .map_err(to_badgateway)?
            .unwrap()
            .implied_price()
            .to_f64()
            .unwrap_or_default(),
        mel_per_dosc,
    };

    let mut body: Body = HomepageTemplate {
        testnet: req.state().netid() == NetID::Testnet,
        blocks,
        pool,
        tooltips: ToolTips {test: InfoBubble ("test tip".into())}
    }
    .render()
    .unwrap()
    .into();
    body.set_mime("text/html");
    Ok(body)
}
