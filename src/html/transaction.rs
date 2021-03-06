use std::{
    collections::{BTreeMap, BTreeSet},
    convert::TryInto,
};

use super::{friendly_denom, MicroUnit, RenderTimeTracer, TOOLTIPS};
use crate::{notfound, to_badgateway, to_badreq, State};
use anyhow::Context;
use askama::Template;
use futures_util::{future::Shared, FutureExt};
use smol::Task;
use themelio_stf::melvm::covenant_weight_from_bytes;
use themelio_structs::{
    Address, CoinData, CoinDataHeight, CoinID, Denom, NetID, Transaction, TxHash,
};

#[derive(Template)]
#[template(path = "transaction.html", escape = "none")]
struct TransactionTemplate {
    testnet: bool,
    txhash: TxHash,
    txhash_abbr: String,
    height: u64,
    transaction: Transaction,
    inputs_with_cdh: Vec<(usize, CoinID, CoinDataHeight, MicroUnit)>,
    outputs: Vec<(usize, CoinData, MicroUnit)>,
    fee: MicroUnit,
    base_fee: MicroUnit,
    tips: MicroUnit,
    net_loss: BTreeMap<String, Vec<MicroUnit>>,
    net_gain: BTreeMap<String, Vec<MicroUnit>>,
    gross_gain: Vec<MicroUnit>,
    tooltips: &'static TOOLTIPS,
}

#[allow(clippy::comparison_chain)]
pub async fn get_txpage(req: tide::Request<State>) -> tide::Result<tide::Response> {
    let _render = RenderTimeTracer::new("txpage");

    let height: u64 = req.param("height").unwrap().parse().map_err(to_badreq)?;
    let txhash: TxHash = TxHash(req.param("txhash").unwrap().parse().map_err(to_badreq)?);
    let snap = req
        .state()
        .val_client
        .snapshot()
        .await
        .map_err(to_badgateway)?
        .get_older(height.into())
        .await
        .map_err(to_badgateway)?;
    let transaction = snap
        .get_transaction(txhash)
        .await
        .map_err(to_badgateway)?
        .ok_or_else(notfound)?;

    let tmapping: BTreeMap<CoinID, Task<anyhow::Result<CoinDataHeight>>> = transaction
        .inputs
        .iter()
        .map(|cid| {
            let cid = *cid;
            let snap = snap.clone();
            (
                cid,
                smolscale::spawn(async move {
                    let cdh = snap
                        .get_coin_spent_here(cid)
                        .await?
                        .context("missing CDH")?;
                    Ok(cdh)
                }),
            )
        })
        .collect();
    let mut coin_map: BTreeMap<CoinID, CoinDataHeight> = BTreeMap::new();
    for (i, (cid, task)) in tmapping.into_iter().enumerate() {
        log::debug!("resolving input {} for {}", i, txhash);
        coin_map.insert(cid, task.await?);
    }

    // now that we have the transaction, we can construct the info.
    let denoms: BTreeSet<_> = transaction.outputs.iter().map(|v| v.denom).collect();
    let mut net_loss: BTreeMap<String, Vec<MicroUnit>> = BTreeMap::new();
    let mut net_gain: BTreeMap<String, Vec<MicroUnit>> = BTreeMap::new();
    for denom in denoms {
        let mut balance: BTreeMap<Address, i128> = BTreeMap::new();
        // we add to the balance
        for output in transaction.outputs.iter() {
            if output.denom == denom {
                let new_balance = balance
                    .get(&output.covhash)
                    .cloned()
                    .unwrap_or_default()
                    .checked_add(output.value.0.try_into()?)
                    .context("cannot add")?;
                balance.insert(output.covhash, new_balance);
            }
        }
        // we subtract from the balance
        for input in transaction.inputs.iter().copied() {
            log::debug!("getting input {} of {}", input, transaction.hash_nosigs());
            let cdh = coin_map[&input].clone();
            if cdh.coin_data.denom == denom {
                let new_balance = balance
                    .get(&cdh.coin_data.covhash)
                    .cloned()
                    .unwrap_or_default()
                    .checked_sub(cdh.coin_data.value.0.try_into()?)
                    .context("cannot add")?;
                balance.insert(cdh.coin_data.covhash, new_balance);
            }
        }
        // we update net loss/gain
        for (addr, balance) in balance {
            if balance < 0 {
                net_loss
                    .entry(addr.0.to_addr())
                    .or_default()
                    .push(MicroUnit((-balance) as u128, friendly_denom(denom)));
            } else if balance > 0 {
                net_gain
                    .entry(addr.0.to_addr())
                    .or_default()
                    .push(MicroUnit(balance as u128, friendly_denom(denom)));
            }
        }
    }

    let fee = transaction.fee;
    let fee_mult = snap
        .get_older((height - 1).into())
        .await
        .map_err(to_badgateway)?
        .current_header()
        .fee_multiplier;
    let base_fee = transaction
        .base_fee(fee_mult, 0, covenant_weight_from_bytes)
        .0;
    let tips = fee.0.saturating_sub(base_fee);

    let mut inputs_with_cdh = vec![];
    // we subtract from the balance
    for (index, input) in transaction.inputs.iter().copied().enumerate() {
        log::debug!("rendering input {} of {}", index, transaction.hash_nosigs());
        let cdh = coin_map[&input].clone();
        inputs_with_cdh.push((
            index,
            input,
            cdh.clone(),
            MicroUnit(
                cdh.coin_data.value.into(),
                friendly_denom(cdh.coin_data.denom),
            ),
        ));
    }

    let mut body: tide::Response = TransactionTemplate {
        testnet: req.state().val_client.netid() == NetID::Testnet,
        txhash,
        txhash_abbr: hex::encode(&txhash.0[..5]),
        height,
        transaction: transaction.clone(),
        net_loss,
        inputs_with_cdh,
        net_gain,
        outputs: transaction
            .outputs
            .iter()
            .enumerate()
            .map(|(i, cd)| {
                (
                    i,
                    cd.clone(),
                    MicroUnit(cd.value.0, friendly_denom(cd.denom)),
                )
            })
            .collect(),
        fee: MicroUnit(fee.0, "MEL".into()),
        base_fee: MicroUnit(base_fee, "MEL".into()),
        tips: MicroUnit(tips, "MEL".into()),
        gross_gain: transaction
            .total_outputs()
            .iter()
            .map(|(denom, val)| MicroUnit(val.0, friendly_denom(*denom)))
            .collect(),
        tooltips: &TOOLTIPS,
    }
    .render()
    .unwrap()
    .into();
    body.set_content_type("text/html");
    body.insert_header("cache-control", "max-age=10000000");
    Ok(body)
}
