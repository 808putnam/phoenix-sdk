use borsh::{BorshDeserialize, BorshSerialize};
use phoenix_types::{
    enums::{SelfTradeBehavior, Side},
    events::MarketEvent,
    instructions::{
        create_cancel_multiple_orders_by_id_instruction, create_cancel_up_to_instruction,
        create_new_order_instruction, CancelMultipleOrdersByIdParams, CancelOrderParams,
        CancelUpToParams,
    },
    market::{FIFOOrderId, TraderState},
    order_packet::OrderPacket,
};
use rand::{rngs::StdRng, Rng};
use solana_sdk::signature::Signature;
use std::{
    collections::BTreeMap,
    fmt::Display,
    ops::{Div, Rem},
    sync::{Arc, Mutex},
};

use anyhow;
use solana_program::{instruction::Instruction, pubkey::Pubkey};

use crate::{
    market_event::{Evict, Fill, FillSummary, MarketEventDetails, PhoenixEvent, Place, Reduce},
    orderbook::Orderbook,
};

const AUDIT_LOG_HEADER_LEN: usize = 92;

pub struct MarketState {
    /// State of the bids and offers in the market.
    pub orderbook: Orderbook<FIFOOrderId, PhoenixOrder>,
    /// Authorized makers in the market.
    pub traders: BTreeMap<Pubkey, TraderState>,
}

#[derive(Clone, Copy, Debug)]
pub struct PhoenixOrder {
    pub num_base_lots: u64,
    pub maker_id: Pubkey,
}

pub fn get_decimal_string<N: Display + Div + Rem + Copy + TryFrom<u64>>(
    amount: N,
    decimals: u32,
) -> String
where
    <N as Rem>::Output: std::fmt::Display,
    <N as Div>::Output: std::fmt::Display,
    <N as TryFrom<u64>>::Error: std::fmt::Debug,
{
    let scale = N::try_from(10_u64.pow(decimals)).unwrap();
    let lhs = amount / scale;
    let rhs = format!("{:0width$}", (amount % scale), width = decimals as usize).replace('-', ""); // remove negative sign from rhs
    format!("{}.{}", lhs, rhs)
}

#[derive(Clone, Copy, Debug)]
pub struct MarketMetadata {
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub base_decimals: u32,
    pub quote_decimals: u32,
    /// 10^base_decimals
    pub base_multiplier: u64,
    /// 10^quote_decimals
    pub quote_multiplier: u64,
    pub quote_lot_size: u64,
    pub base_lot_size: u64,
    pub tick_size: u64,
    pub num_base_lots_per_base_unit: u64,
    pub num_quote_lots_per_tick: u64,
}

#[derive(Debug, Copy, Clone, BorshDeserialize, BorshSerialize)]
pub enum MarketEventWrapper {
    Uninitialized,
    Header,
    Fill,
    Place,
    Reduce,
    Evict,
    FillSummary,
}

pub struct SDKClientCore {
    pub markets: BTreeMap<Pubkey, MarketMetadata>,
    pub rng: Arc<Mutex<StdRng>>,
    pub active_market_key: Pubkey,
    pub trader: Pubkey,
}

impl SDKClientCore {
    pub fn base_units_to_base_lots(&self, base_units: f64) -> u64 {
        let market = self.markets.get(&self.active_market_key).unwrap();
        (base_units * market.base_multiplier as f64 / market.base_lot_size as f64) as u64
    }

    pub fn base_amount_to_base_lots(&self, base_amount: u64) -> u64 {
        let market = self.markets.get(&self.active_market_key).unwrap();
        base_amount / market.base_lot_size
    }

    pub fn base_lots_to_base_amount(&self, base_lots: u64) -> u64 {
        let market = self.markets.get(&self.active_market_key).unwrap();
        base_lots * market.base_lot_size
    }

    pub fn quote_units_to_quote_lots(&self, quote_units: f64) -> u64 {
        let market = self.markets.get(&self.active_market_key).unwrap();
        (quote_units * market.quote_multiplier as f64 / market.quote_lot_size as f64) as u64
    }

    pub fn quote_amount_to_quote_lots(&self, quote_amount: u64) -> u64 {
        let market = self.markets.get(&self.active_market_key).unwrap();
        quote_amount / market.quote_lot_size
    }

    pub fn quote_lots_to_quote_amount(&self, quote_lots: u64) -> u64 {
        let market = self.markets.get(&self.active_market_key).unwrap();
        quote_lots * market.quote_lot_size
    }

    pub fn base_amount_to_base_unit_as_float(&self, base_amount: u64) -> f64 {
        let market = self.markets.get(&self.active_market_key).unwrap();
        base_amount as f64 / market.base_multiplier as f64
    }

    pub fn quote_amount_to_quote_unit_as_float(&self, quote_amount: u64) -> f64 {
        let market = self.markets.get(&self.active_market_key).unwrap();
        quote_amount as f64 / market.quote_multiplier as f64
    }

    pub fn print_quote_amount(&self, quote_amount: u64) {
        let market = self.markets.get(&self.active_market_key).unwrap();
        println!(
            "{}",
            get_decimal_string(quote_amount, market.quote_decimals)
        );
    }

    pub fn print_base_amount(&self, base_amount: u64) {
        let market = self.markets.get(&self.active_market_key).unwrap();
        println!("{}", get_decimal_string(base_amount, market.base_decimals));
    }

    pub fn fill_event_to_quote_amount(&self, fill: &Fill) -> u64 {
        let &Fill {
            base_lots_filled: base_lots,
            price_in_ticks,
            ..
        } = fill;
        self.order_to_quote_amount(base_lots, price_in_ticks)
    }

    pub fn order_to_quote_amount(&self, base_lots: u64, price_in_ticks: u64) -> u64 {
        let market = self.markets.get(&self.active_market_key).unwrap();
        base_lots * price_in_ticks * market.num_quote_lots_per_tick * market.quote_lot_size
            / market.num_base_lots_per_base_unit
    }

    pub fn float_price_to_ticks(&self, price: f64) -> u64 {
        let meta = self.get_active_market_metadata();
        ((price * meta.quote_multiplier as f64) / meta.tick_size as f64) as u64
    }

    pub fn float_price_to_ticks_rounded_up(&self, price: f64) -> u64 {
        let meta = self.get_active_market_metadata();
        ((price * meta.quote_multiplier as f64) / meta.tick_size as f64).ceil() as u64
    }

    pub fn ticks_to_float_price(&self, ticks: u64) -> f64 {
        let meta = self.get_active_market_metadata();
        (ticks as f64 * meta.tick_size as f64) / meta.quote_multiplier as f64
    }

    pub fn base_lots_to_base_units_multiplier(&self) -> f64 {
        let market = self.markets.get(&self.active_market_key).unwrap();
        1.0 / market.num_base_lots_per_base_unit as f64
    }

    pub fn ticks_to_float_price_multiplier(&self) -> f64 {
        let meta = self.get_active_market_metadata();
        meta.tick_size as f64 / meta.quote_multiplier as f64
    }
}

impl SDKClientCore {
    pub fn get_next_client_order_id(&self) -> u128 {
        self.rng.lock().unwrap().gen::<u128>()
    }

    pub fn change_active_market(&mut self, market: &Pubkey) -> anyhow::Result<()> {
        if self.markets.get(market).is_some() {
            self.active_market_key = *market;
            Ok(())
        } else {
            Err(anyhow::Error::msg("Market not found"))
        }
    }

    pub fn get_active_market_metadata(&self) -> &MarketMetadata {
        self.markets.get(&self.active_market_key).unwrap()
    }

    pub fn parse_wrapper_events(
        &self,
        sig: &Signature,
        events: Vec<Vec<u8>>,
    ) -> Option<Vec<PhoenixEvent>> {
        let mut market_events: Vec<PhoenixEvent> = vec![];
        let meta = self.get_active_market_metadata();

        for event in events.iter() {
            let num_bytes = event.len();
            let header_event = MarketEvent::try_from_slice(&event[..AUDIT_LOG_HEADER_LEN]).ok()?;
            let header = match header_event {
                MarketEvent::Header { header } => Some(header),
                _ => {
                    panic!("Expected a header event");
                }
            }?;
            let mut offset = AUDIT_LOG_HEADER_LEN;
            while offset < num_bytes {
                match MarketEventWrapper::try_from_slice(&[event[offset]]).ok()? {
                    MarketEventWrapper::Fill => {
                        let size = 67;
                        let fill_event =
                            MarketEvent::try_from_slice(&event[offset..offset + size]).ok()?;
                        offset += size;

                        match fill_event {
                            MarketEvent::Fill {
                                index,
                                maker_id,
                                order_sequence_number,
                                price_in_ticks,
                                base_lots_filled,
                                base_lots_remaining,
                            } => market_events.push(PhoenixEvent {
                                market: header.market,
                                sequence_number: header.market_sequence_number,
                                slot: header.slot,
                                timestamp: header.timestamp,
                                signature: *sig,
                                signer: header.signer,
                                event_index: index as u64,
                                details: MarketEventDetails::Fill(Fill {
                                    order_sequence_number,
                                    maker: maker_id,
                                    taker: header.signer,
                                    price_in_ticks,
                                    base_lots_filled,
                                    base_lots_remaining,
                                    side_filled: Side::from_order_sequence_number(
                                        order_sequence_number,
                                    ),
                                    is_full_fill: base_lots_remaining == 0,
                                }),
                            }),
                            _ => panic!("Expected a fill event"),
                        };
                    }

                    MarketEventWrapper::Reduce => {
                        let size = 35;
                        let reduce_event =
                            MarketEvent::try_from_slice(&event[offset..offset + size]).ok()?;
                        offset += size;

                        match reduce_event {
                            MarketEvent::Reduce {
                                index,
                                order_sequence_number,
                                price_in_ticks,
                                base_lots_removed,
                                base_lots_remaining,
                            } => market_events.push(PhoenixEvent {
                                market: header.market,
                                sequence_number: header.market_sequence_number,
                                slot: header.slot,
                                timestamp: header.timestamp,
                                signature: *sig,
                                signer: header.signer,
                                event_index: index as u64,
                                details: MarketEventDetails::Reduce(Reduce {
                                    order_sequence_number,
                                    maker: header.signer,
                                    price_in_ticks,
                                    base_lots_removed,
                                    base_lots_remaining,
                                    is_full_cancel: base_lots_remaining == 0,
                                }),
                            }),
                            _ => {
                                panic!("Expected a reduce event");
                            }
                        };
                    }

                    MarketEventWrapper::Place => {
                        let size = 43;
                        let place_event =
                            MarketEvent::try_from_slice(&event[offset..offset + size]).ok()?;
                        offset += size;

                        match place_event {
                            MarketEvent::Place {
                                index,
                                order_sequence_number,
                                client_order_id,
                                price_in_ticks,
                                base_lots_placed,
                            } => market_events.push(PhoenixEvent {
                                market: header.market,
                                sequence_number: header.market_sequence_number,
                                slot: header.slot,
                                timestamp: header.timestamp,
                                signature: *sig,
                                signer: header.signer,
                                event_index: index as u64,
                                details: MarketEventDetails::Place(Place {
                                    order_sequence_number,
                                    client_order_id,
                                    maker: header.signer,
                                    price_in_ticks,
                                    base_lots_placed,
                                }),
                            }),
                            _ => {
                                panic!("Expected a place event");
                            }
                        };
                    }

                    MarketEventWrapper::Evict => {
                        let size = 58;
                        let evict_event =
                            MarketEvent::try_from_slice(&event[offset..offset + size]).ok()?;
                        offset += size;

                        match evict_event {
                            MarketEvent::Evict {
                                index,
                                maker_id,
                                order_sequence_number,
                                price_in_ticks,
                                base_lots_evicted,
                            } => market_events.push(PhoenixEvent {
                                market: header.market,
                                sequence_number: header.market_sequence_number,
                                slot: header.slot,
                                timestamp: header.timestamp,
                                signature: *sig,
                                signer: header.signer,
                                event_index: index as u64,
                                details: MarketEventDetails::Evict(Evict {
                                    order_sequence_number,
                                    maker: maker_id,
                                    price_in_ticks,
                                    base_lots_evicted,
                                }),
                            }),
                            _ => {
                                panic!("Expected a place event");
                            }
                        };
                    }
                    MarketEventWrapper::FillSummary => {
                        let size = 43;
                        let fill_summary_event =
                            MarketEvent::try_from_slice(&event[offset..offset + size]).ok()?;
                        offset += size;
                        println!("Fill summary event: {:?}", fill_summary_event);

                        match fill_summary_event {
                            MarketEvent::FillSummary {
                                index,
                                client_order_id,
                                total_base_lots_filled,
                                total_quote_lots_filled,
                                total_fee_in_quote_lots,
                            } => market_events.push(PhoenixEvent {
                                market: header.market,
                                sequence_number: header.market_sequence_number,
                                slot: header.slot,
                                timestamp: header.timestamp,
                                signature: *sig,
                                signer: header.signer,
                                event_index: index as u64,
                                details: MarketEventDetails::FillSummary(FillSummary {
                                    client_order_id,
                                    total_base_filled: total_base_lots_filled * meta.base_lot_size,
                                    total_quote_filled_including_fees: total_quote_lots_filled
                                        * meta.quote_lot_size,
                                    total_quote_fees: total_fee_in_quote_lots * meta.quote_lot_size,
                                }),
                            }),
                            _ => {
                                panic!("Expected fill summary event");
                            }
                        };
                    }

                    _ => {
                        panic!("Unexpected Event!");
                    }
                }
            }
        }
        Some(market_events)
    }

    pub fn get_ioc_ix(&self, price: u64, side: Side, num_base_lots: u64) -> Instruction {
        self.get_ioc_generic_ix(price, side, num_base_lots, None, None, None, None)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn get_ioc_generic_ix(
        &self,
        price: u64,
        side: Side,
        num_base_lots: u64,
        self_trade_behavior: Option<SelfTradeBehavior>,
        match_limit: Option<u64>,
        client_order_id: Option<u128>,
        use_only_deposited_funds: Option<bool>,
    ) -> Instruction {
        let meta = &self.markets[&self.active_market_key];
        let num_quote_ticks_per_base_unit = price / meta.tick_size;
        let self_trade_behavior = self_trade_behavior.unwrap_or(SelfTradeBehavior::CancelProvide);
        let client_order_id = client_order_id.unwrap_or(0);
        let use_only_deposited_funds = use_only_deposited_funds.unwrap_or(false);
        create_new_order_instruction(
            &self.active_market_key.clone(),
            &self.trader,
            &meta.base_mint,
            &meta.quote_mint,
            &OrderPacket::new_ioc_by_lots(
                side,
                num_quote_ticks_per_base_unit,
                num_base_lots,
                self_trade_behavior,
                match_limit,
                client_order_id,
                use_only_deposited_funds,
            ),
        )
    }

    pub fn get_fok_sell_ix(&self, price: u64, size_in_base_lots: u64) -> Instruction {
        self.get_fok_generic_ix(price, Side::Ask, size_in_base_lots, None, None, None, None)
    }

    pub fn get_fok_buy_generic_ix(
        &self,
        price: u64,
        size_in_quote_lots: u64,
        self_trade_behavior: Option<SelfTradeBehavior>,
        match_limit: Option<u64>,
        client_order_id: Option<u128>,
        use_only_deposited_funds: Option<bool>,
    ) -> Instruction {
        self.get_fok_generic_ix(
            price,
            Side::Bid,
            size_in_quote_lots,
            self_trade_behavior,
            match_limit,
            client_order_id,
            use_only_deposited_funds,
        )
    }

    pub fn get_fok_sell_generic_ix(
        &self,
        price: u64,
        size_in_base_lots: u64,
        self_trade_behavior: Option<SelfTradeBehavior>,
        match_limit: Option<u64>,
        client_order_id: Option<u128>,
        use_only_deposited_funds: Option<bool>,
    ) -> Instruction {
        self.get_fok_generic_ix(
            price,
            Side::Ask,
            size_in_base_lots,
            self_trade_behavior,
            match_limit,
            client_order_id,
            use_only_deposited_funds,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn get_fok_generic_ix(
        &self,
        price: u64,
        side: Side,
        size: u64,
        self_trade_behavior: Option<SelfTradeBehavior>,
        match_limit: Option<u64>,
        client_order_id: Option<u128>,
        use_only_deposited_funds: Option<bool>,
    ) -> Instruction {
        let meta = &self.markets[&self.active_market_key];
        let self_trade_behavior = self_trade_behavior.unwrap_or(SelfTradeBehavior::CancelProvide);
        let client_order_id = client_order_id.unwrap_or(0);
        let target_price_in_ticks = price / meta.tick_size;
        let use_only_deposited_funds = use_only_deposited_funds.unwrap_or(false);
        match side {
            Side::Bid => {
                let quote_lot_budget = size / meta.quote_lot_size;
                create_new_order_instruction(
                    &self.active_market_key.clone(),
                    &self.trader,
                    &meta.base_mint,
                    &meta.quote_mint,
                    &OrderPacket::new_fok_buy_with_limit_price(
                        target_price_in_ticks,
                        quote_lot_budget,
                        self_trade_behavior,
                        match_limit,
                        client_order_id,
                        use_only_deposited_funds,
                    ),
                )
            }
            Side::Ask => {
                let num_base_lots = size / meta.base_lot_size;
                create_new_order_instruction(
                    &self.active_market_key.clone(),
                    &self.trader,
                    &meta.base_mint,
                    &meta.quote_mint,
                    &OrderPacket::new_fok_sell_with_limit_price(
                        target_price_in_ticks,
                        num_base_lots,
                        self_trade_behavior,
                        match_limit,
                        client_order_id,
                        use_only_deposited_funds,
                    ),
                )
            }
        }
    }

    pub fn get_ioc_with_slippage_ix(
        &self,
        lots_in: u64,
        min_lots_out: u64,
        side: Side,
    ) -> Instruction {
        let meta = self.get_active_market_metadata();

        let order_type = match side {
            Side::Bid => OrderPacket::new_ioc_buy_with_slippage(lots_in, min_lots_out),
            Side::Ask => OrderPacket::new_ioc_sell_with_slippage(lots_in, min_lots_out),
        };

        create_new_order_instruction(
            &self.active_market_key.clone(),
            &self.trader,
            &meta.base_mint,
            &meta.quote_mint,
            &order_type,
        )
    }

    pub fn get_ioc_from_tick_price_ix(
        &self,
        tick_price: u64,
        side: Side,
        size: u64,
    ) -> Instruction {
        let meta = &self.markets[&self.active_market_key];

        create_new_order_instruction(
            &self.active_market_key.clone(),
            &self.trader,
            &meta.base_mint,
            &meta.quote_mint,
            &OrderPacket::new_ioc_by_lots(
                side,
                tick_price,
                size,
                SelfTradeBehavior::CancelProvide,
                None,
                self.rng.lock().unwrap().gen::<u128>(),
                false,
            ),
        )
    }

    pub fn get_post_only_ix(&self, price: u64, side: Side, size: u64) -> Instruction {
        self.get_post_only_generic_ix(price, side, size, None, None, None)
    }

    pub fn get_post_only_generic_ix(
        &self,
        price: u64,
        side: Side,
        size: u64,
        client_order_id: Option<u128>,
        reject_post_only: Option<bool>,
        use_only_deposited_funds: Option<bool>,
    ) -> Instruction {
        let meta = &self.markets[&self.active_market_key];
        let price_in_ticks = price / meta.tick_size;
        let client_order_id = client_order_id.unwrap_or(0);
        let reject_post_only = reject_post_only.unwrap_or(false);
        let use_only_deposited_funds = use_only_deposited_funds.unwrap_or(false);
        create_new_order_instruction(
            &self.active_market_key.clone(),
            &self.trader,
            &meta.base_mint,
            &meta.quote_mint,
            &OrderPacket::new_post_only(
                side,
                price_in_ticks,
                size,
                client_order_id,
                reject_post_only,
                use_only_deposited_funds,
            ),
        )
    }

    pub fn get_post_only_ix_from_tick_price(
        &self,
        tick_price: u64,
        side: Side,
        size: u64,
        client_order_id: u128,
        improve_price_on_cross: bool,
    ) -> Instruction {
        let meta = &self.markets[&self.active_market_key];
        create_new_order_instruction(
            &self.active_market_key.clone(),
            &self.trader,
            &meta.base_mint,
            &meta.quote_mint,
            &if improve_price_on_cross {
                OrderPacket::new_adjustable_post_only_default_with_client_order_id(
                    side,
                    tick_price,
                    size,
                    client_order_id,
                )
            } else {
                OrderPacket::new_post_only_default_with_client_order_id(
                    side,
                    tick_price,
                    size,
                    client_order_id,
                )
            },
        )
    }

    pub fn get_limit_order_ix(&self, price: u64, side: Side, size: u64) -> Instruction {
        self.get_limit_order_generic_ix(price, side, size, None, None, None, None)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn get_limit_order_generic_ix(
        &self,
        price: u64,
        side: Side,
        size: u64,
        self_trade_behavior: Option<SelfTradeBehavior>,
        match_limit: Option<u64>,
        client_order_id: Option<u128>,
        use_only_deposited_funds: Option<bool>,
    ) -> Instruction {
        let meta = &self.markets[&self.active_market_key];
        let num_quote_ticks_per_base_unit = price / meta.tick_size;
        let self_trade_behavior = self_trade_behavior.unwrap_or(SelfTradeBehavior::DecrementTake);
        let client_order_id = client_order_id.unwrap_or(0);
        let use_only_deposited_funds = use_only_deposited_funds.unwrap_or(false);
        create_new_order_instruction(
            &self.active_market_key.clone(),
            &self.trader,
            &meta.base_mint,
            &meta.quote_mint,
            &OrderPacket::new_limit_order(
                side,
                num_quote_ticks_per_base_unit,
                size,
                self_trade_behavior,
                match_limit,
                client_order_id,
                use_only_deposited_funds,
            ),
        )
    }

    pub fn get_limit_order_ix_from_tick_price(
        &self,
        tick_price: u64,
        side: Side,
        size: u64,
        client_order_id: u128,
    ) -> Instruction {
        let meta = &self.markets[&self.active_market_key];
        create_new_order_instruction(
            &self.active_market_key.clone(),
            &self.trader,
            &meta.base_mint,
            &meta.quote_mint,
            &OrderPacket::new_limit_order_default_with_client_order_id(
                side,
                tick_price,
                size,
                client_order_id,
            ),
        )
    }

    pub fn get_cancel_ids_ix(&self, ids: Vec<FIFOOrderId>) -> Instruction {
        let mut cancel_orders = vec![];
        for &FIFOOrderId {
            num_quote_ticks_per_base_unit,
            order_sequence_number,
        } in ids.iter()
        {
            cancel_orders.push(CancelOrderParams {
                side: Side::from_order_sequence_number(order_sequence_number),
                num_quote_ticks_per_base_unit,
                order_id: order_sequence_number,
            });
        }
        let meta = &self.markets[&self.active_market_key];
        let cancel_multiple_orders = CancelMultipleOrdersByIdParams {
            orders: cancel_orders,
        };

        create_cancel_multiple_orders_by_id_instruction(
            &self.active_market_key.clone(),
            &self.trader,
            &meta.base_mint,
            &meta.quote_mint,
            &cancel_multiple_orders,
        )
    }

    pub fn get_cancel_up_to_ix(&self, tick_limit: Option<u64>, side: Side) -> Instruction {
        let params = CancelUpToParams {
            side,
            tick_limit,
            num_orders_to_search: None,
            num_orders_to_cancel: None,
        };

        let meta = &self.markets[&self.active_market_key];
        create_cancel_up_to_instruction(
            &self.active_market_key.clone(),
            &self.trader,
            &meta.base_mint,
            &meta.quote_mint,
            &params,
        )
    }
}
