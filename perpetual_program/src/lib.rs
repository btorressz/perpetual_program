use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    program::invoke_signed,
    system_instruction,
};
use anchor_spl::token::{self, Token, TokenAccount, Transfer, Mint};

//  placeholders for  oracle usage
use pyth_sdk_solana::load_price_feed_from_account_info;
// use switchboard_v2::AggregatorAccountData;



// program id
declare_id!("6QZ2P8VX7ENknVJJ4Tgm5ZbVAzCiL6daW349FhTG8PW7");

// =======================================
// PROGRAM
// =======================================
#[program]
pub mod perpetual_program {
    use super::*;

    ////////////////////////////////////////////////////////////////////////////
    // MULTI-ASSET COLLATERAL SUPPORT (SOL, USDC)
    ////////////////////////////////////////////////////////////////////////////
    // Store a list of accepted collaterals in MarketState, plus some logic.
    // For demonstration purposes, USDC and SOL are shown. If using wSOL, treat it as an SPL token.  

    /// Initialize the market, create PDAs for fee & insurance vaults, etc.
    pub fn initialize_market(
        ctx: Context<InitializeMarket>,
        initial_funding_rate: i64,
        base_asset_symbol: String,
        quote_asset_mint: Pubkey, // The primary SPL token used for collateral
    ) -> Result<()> {
        let market_state = &mut ctx.accounts.market_state;

        market_state.authority = *ctx.accounts.authority.key;
        market_state.base_asset_symbol = base_asset_symbol;
        market_state.quote_asset_mint = quote_asset_mint;

        market_state.funding_rate = initial_funding_rate;
        market_state.last_funding_time = Clock::get()?.unix_timestamp;

        // Maintenance margin ratio in basis points (50 => 5%)
        market_state.maintenance_margin_ratio_bps = 50;

        // Additional dynamic margin base ratio
        market_state.base_margin_ratio_bps = 50;

        // Turn on auto-deleverage by default
        market_state.auto_deleverage_enabled = true;

        // PDAs for fee & insurance
        market_state.fee_vault = ctx.accounts.fee_vault.key();
        market_state.insurance_vault = ctx.accounts.insurance_vault.key();

        market_state.open_interest_long = 0;
        market_state.open_interest_short = 0;
        market_state.index_price = 1000;

        // For Dutch auction liquidation
        market_state.dutch_auction_discount_bps = 0; // Start at 0 => no discount initially

        msg!("Market initialized. Multi-asset framework is in place.");
        Ok(())
    }

     /// Deposits collateral into a user-specific vault (PDA) for this market.
    /// Optional logic is available for multi-asset support. 
    /// For demonstration purposes, the assumption is that the user can deposit 
    /// USDC or wSOL, with the token's mint located in the user_collateral_account.
    pub fn deposit_collateral(ctx: Context<DepositCollateral>, amount: u64) -> Result<()> {
        require!(amount > 0, PerpError::InvalidAmount);

        // Transfer from user to user vault
        let cpi_accounts = Transfer {
            from: ctx.accounts.user_collateral_account.to_account_info(),
            to: ctx.accounts.user_vault.to_account_info(),
            authority: ctx.accounts.user.to_account_info(),
        };
        let cpi_ctx = CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
        token::transfer(cpi_ctx, amount)?;

        // Track user position
        let user_position = &mut ctx.accounts.user_position;
        user_position.user = ctx.accounts.user.key();
        user_position.market = ctx.accounts.market_state.key();
        user_position.collateral = user_position
            .collateral
            .checked_add(amount)
            .ok_or(PerpError::MathOverflow)?;

        emit!(CollateralDeposited {
            user: user_position.user,
            amount,
        });

        Ok(())
    }

    /// Withdraws collateral. Partial withdrawals are allowed as long as they do not break margin requirements.  
    pub fn withdraw_collateral(ctx: Context<WithdrawCollateral>, amount: u64) -> Result<()> {
        require!(amount > 0, PerpError::InvalidAmount);

        let user_position = &mut ctx.accounts.user_position;

        // Check margin requirement
        let (margin_ok, _) = is_margin_healthy(user_position, &ctx.accounts.market_state, None);
        require!(margin_ok, PerpError::InsufficientMargin);

        require!(user_position.collateral >= amount, PerpError::InsufficientCollateral);

        let cpi_accounts = Transfer {
            from: ctx.accounts.user_vault.to_account_info(),
            to: ctx.accounts.user_collateral_account.to_account_info(),
            authority: ctx.accounts.user_vault_authority.to_account_info(),
        };

        let market_key = ctx.accounts.market_state.key();
        let seeds = &[
            b"user_vault",
            user_position.user.as_ref(),
            market_key.as_ref(),
            &[ctx.bumps.user_vault_authority],
        ];
        let signer = &[&seeds[..]];

        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
            signer,
        );
        token::transfer(cpi_ctx, amount)?;

        user_position.collateral = user_position
            .collateral
            .checked_sub(amount)
            .ok_or(PerpError::MathOverflow)?;

        emit!(CollateralWithdrawn {
            user: user_position.user,
            amount,
        });

        Ok(())
    }

    ////////////////////////////////////////////////////////////////////////////
    //  OCO & Bracket Orders for HFT traders
    ////////////////////////////////////////////////////////////////////////////
    // The advanced order logic will be expanded to accept a bracket of (stop_loss, take_profit).
    
    /// Place a bracket order that includes both stop_loss and take_profit.
    /// For demonstration purposes, they are stored in a new bracket order struct.  
    pub fn place_bracket_order(
        ctx: Context<PlaceBracketOrder>,
        stop_loss_price: u64,
        take_profit_price: u64,
    ) -> Result<()> {
        let bracket_order = &mut ctx.accounts.bracket_order;
        bracket_order.user = ctx.accounts.user.key();
        bracket_order.market = ctx.accounts.market_state.key();
        bracket_order.stop_loss_price = stop_loss_price;
        bracket_order.take_profit_price = take_profit_price;
        bracket_order.size = ctx.accounts.user_position.size;
        bracket_order.is_long = ctx.accounts.user_position.is_long;

      // If user_position is 0 or not valid, the bracket order is meaningless, but for demonstration purposes, this is ignored.  

        msg!("Placed bracket order: stop_loss = {}, tp = {}", stop_loss_price, take_profit_price);
        Ok(())
    }

    /// Trigger bracket order if conditions met (like stop_loss or take_profit).
    /// If one trigger condition is met, the position is closed.
    /// The other is effectively canceled.

    pub fn trigger_bracket_order(ctx: Context<TriggerBracketOrder>) -> Result<()> {
        let bracket_order = &mut ctx.accounts.bracket_order;
        let user_position = &mut ctx.accounts.user_position;
        let market_state = &mut ctx.accounts.market_state;

        require!(user_position.size > 0, PerpError::NoOpenPosition);

        // Check current price
        let current_price = get_oracle_price(&ctx.accounts.oracle_price_feed_account)?;
        let is_long = bracket_order.is_long;
        // If is_long => stop_loss triggers if price <= bracket_order.stop_loss_price,
        // or take_profit if price >= bracket_order.take_profit_price.

        let mut triggered = false;

        if is_long {
            if current_price <= bracket_order.stop_loss_price {
                msg!("Stop loss triggered.");
                triggered = true;
            } else if current_price >= bracket_order.take_profit_price {
                msg!("Take profit triggered.");
                triggered = true;
            }
        } else {
            // short position
            if current_price >= bracket_order.stop_loss_price {
                msg!("Stop loss triggered (short). ");
                triggered = true;
            } else if current_price <= bracket_order.take_profit_price {
                msg!("Take profit triggered (short). ");
                triggered = true;
            }
        }

        if !triggered {
            return Ok(()); // no trigger
        }

        // If triggered, close position.
        let direction_multiplier = if user_position.is_long { 1 } else { -1 };
        let realized_pnl = (user_position.size as i64)
            .checked_mul((current_price as i64 - user_position.entry_price as i64))
            .ok_or(PerpError::MathOverflow)?
            .checked_mul(direction_multiplier)
            .ok_or(PerpError::MathOverflow)?;

        let new_collateral = (user_position.collateral as i64)
            .checked_add(realized_pnl)
            .ok_or(PerpError::MathOverflow)?;
        user_position.collateral = if new_collateral < 0 { 0 } else { new_collateral as u64 };

        // Update open interest
        if user_position.is_long {
            market_state.open_interest_long = market_state
                .open_interest_long
                .checked_sub(user_position.size)
                .unwrap_or_default();
        } else {
            market_state.open_interest_short = market_state
                .open_interest_short
                .checked_sub(user_position.size)
                .unwrap_or_default();
        }

        // Reset user position
        user_position.size = 0;
        user_position.entry_price = 0;
        user_position.is_long = false;
        user_position.unrealized_pnl = 0;

        // Mark bracket as used
        bracket_order.size = 0;
        msg!("Bracket order executed, position closed.");
        Ok(())
    }

    ////////////////////////////////////////////////////////////////////////////
    //  ADAPTIVE FUNDING RATE BASED ON OPEN INTEREST
    ////////////////////////////////////////////////////////////////////////////
    /// The update_funding_rate function will be adjusted
    /// to factor in open interest (OI) imbalance.

    pub fn update_funding_rate(ctx: Context<UpdateFundingRate>) -> Result<()> {
        let market_state = &mut ctx.accounts.market_state;
        let now = Clock::get()?.unix_timestamp;

        let time_diff = (now - market_state.last_funding_time).max(0);
        if time_diff == 0 {
            return Ok(());
        }

        let mark_price = get_oracle_price(&ctx.accounts.oracle_price_feed_account)?;
        let index_price = market_state.index_price;

        let diff = mark_price as i64 - index_price as i64;
        // This naive formula used to do (diff / 10) * time_diff.
        // Open interest is now factored in. If OI long > OI short, longs incur higher charges.

        let oi_long = market_state.open_interest_long as i64;
        let oi_short = market_state.open_interest_short as i64;
        let oi_diff = oi_long.checked_sub(oi_short).unwrap_or_default();

        // If oi_diff > 0 => more longs => funding rate is positive => longs pay.
        // If oi_diff < 0 => more shorts => negative => shorts pay.

        let base_rate = (diff / 10).checked_mul(time_diff as i64).unwrap_or_default();
        // An OI factor is included, e.g., 1 basis point per 100 difference in OI.

        let oi_factor = (oi_diff / 100).max(-1000).min(1000); // clamp for safety
        let new_funding_rate = base_rate + oi_factor;

        market_state.funding_rate = new_funding_rate;
        market_state.last_funding_time = now;

        emit!(FundingRateUpdated {
            market: market_state.key(),
            new_funding_rate,
        });

        Ok(())
    }

    ////////////////////////////////////////////////////////////////////////////
    //  LIQUIDATION AUTOMATION (For Future Keepers/Bots)
    ////////////////////////////////////////////////////////////////////////////
    ////Liquidation can be called by anyone, but it is primarily designed for a keeper.
    /// Future integration with Switchboard could enable automatic execution of this function.

    pub fn liquidate_position(ctx: Context<LiquidatePosition>, liquidation_size: u64) -> Result<()> {
        let market_state = &mut ctx.accounts.market_state;
        let user_position = &mut ctx.accounts.user_position;

        require!(user_position.size > 0, PerpError::NoOpenPosition);
        require!(liquidation_size > 0, PerpError::InvalidAmount);
        require!(liquidation_size <= user_position.size, PerpError::InvalidAmount);

        let (margin_ok, net_equity) = is_margin_healthy(user_position, market_state, None);
        if margin_ok {
            return err!(PerpError::PositionNotLiquidatable);
        }

        let discount_level_bps = market_state.dutch_auction_discount_bps;
        let liquidator_reward_bps = 100; // 10%
        let current_mark_price = get_oracle_price(&ctx.accounts.oracle_price_feed_account)?;
        let direction_multiplier = if user_position.is_long { 1 } else { -1 };

        let partial_pnl = (liquidation_size as i64)
            .checked_mul((current_mark_price as i64 - user_position.entry_price as i64))
            .ok_or(PerpError::MathOverflow)?
            .checked_mul(direction_multiplier)
            .ok_or(PerpError::MathOverflow)?;

        let new_collateral_i64 = (user_position.collateral as i64)
            .checked_add(partial_pnl)
            .ok_or(PerpError::MathOverflow)?;

        // Dutch Auction discount.
        let discount_amount = (new_collateral_i64
            .checked_mul(discount_level_bps as i64)
            .unwrap_or(0))
            .checked_div(1000)
            .unwrap_or(0);
        let discounted_collateral = new_collateral_i64.checked_sub(discount_amount).unwrap_or(0);
        let liquidator_reward = (discount_amount
            .checked_mul(liquidator_reward_bps as i64)
            .unwrap_or(0))
            .checked_div(1000)
            .unwrap_or(0);

        let final_collateral = if discounted_collateral < 0 {
            0
        } else {
            discounted_collateral as u64
        };

        user_position.collateral = final_collateral;
        user_position.size = user_position.size.checked_sub(liquidation_size).unwrap_or(0);

        if user_position.size == 0 {
            user_position.entry_price = 0;
            user_position.is_long = false;
            user_position.unrealized_pnl = 0;
        }

        if user_position.is_long {
            market_state.open_interest_long = market_state
                .open_interest_long
                .checked_sub(liquidation_size)
                .unwrap_or_default();
        } else {
            market_state.open_interest_short = market_state
                .open_interest_short
                .checked_sub(liquidation_size)
                .unwrap_or_default();
        }

        // Increase discount for next time.
        market_state.dutch_auction_discount_bps = market_state
            .dutch_auction_discount_bps
            .checked_add(50)
            .unwrap_or(1000);

        emit!(PositionLiquidated {
            user: user_position.user,
            market: user_position.market,
            penalty: discount_amount,
            liquidation_size,
        });
        msg!("Liquidator reward: {}", liquidator_reward);

        // Potentially integrate with Switchboard here for automation.
        if market_state.auto_deleverage_enabled {
            handle_auto_deleveraging(market_state)?;
        }

        Ok(())
    }

    ////////////////////////////////////////////////////////////////////////////
    //  SMART LEVERAGE LIMITS (RISK & VOLATILITY)
    ////////////////////////////////////////////////////////////////////////////
    /// A simple approach will be incorporated in is_margin_healthy.
    /// Dynamic margin logic has already been partially implemented. A maximum leverage check is now added.

    /// Open or extend a position (overridden) with new leverage check.
    pub fn open_position(ctx: Context<OpenPosition>, is_long: bool, size: u64) -> Result<()> {
        let market_state = &mut ctx.accounts.market_state;
        let user_position = &mut ctx.accounts.user_position;

        require!(size > 0, PerpError::InvalidAmount);

         // A basic approach assumes max_leverage = 10.
        // Then user_position.collateral * 10 >= size * current_price.
        let current_mark_price = 1000; // placeholder
        let max_leverage = 10_u64;
        let cost = size.checked_mul(current_mark_price).ok_or(PerpError::MathOverflow)?;
        let max_allowed = user_position
            .collateral
            .checked_mul(max_leverage)
            .ok_or(PerpError::MathOverflow)?;
        require!(cost <= max_allowed, PerpError::InsufficientMargin);

        if user_position.size == 0 {
            user_position.is_long = is_long;
            user_position.entry_price = market_state.index_price;
            user_position.size = size;
        } else {
            require!(user_position.is_long == is_long, PerpError::OppositePositionNotSupported);
            let old_size = user_position.size;
            let old_entry_price = user_position.entry_price;
            let total_size = old_size.checked_add(size).ok_or(PerpError::MathOverflow)?;
            let new_entry_price = (old_entry_price as u128)
                .checked_mul(old_size as u128)
                .ok_or(PerpError::MathOverflow)?
                .checked_add(
                    (market_state.index_price as u128)
                        .checked_mul(size as u128)
                        .ok_or(PerpError::MathOverflow)?,
                )
                .ok_or(PerpError::MathOverflow)?
                .checked_div(total_size as u128)
                .ok_or(PerpError::MathOverflow)? as u64;

            user_position.entry_price = new_entry_price;
            user_position.size = total_size;
        }

        // Update OI(open interest)
        if is_long {
            market_state.open_interest_long = market_state
                .open_interest_long
                .checked_add(size)
                .ok_or(PerpError::MathOverflow)?;
        } else {
            market_state.open_interest_short = market_state
                .open_interest_short
                .checked_add(size)
                .ok_or(PerpError::MathOverflow)?;
        }

        // Final margin check
        let (margin_ok, _) = is_margin_healthy(user_position, market_state, None);
        require!(margin_ok, PerpError::InsufficientMargin);

        emit!(PositionOpened {
            user: user_position.user,
            market: user_position.market,
            size: user_position.size,
            is_long: user_position.is_long,
        });

        Ok(())
    }

    /// Close an existing position (fully) remains the same.
    pub fn close_position(ctx: Context<ClosePosition>) -> Result<()> {
        let user_position = &mut ctx.accounts.user_position;
        let market_state = &mut ctx.accounts.market_state;

        require!(user_position.size > 0, PerpError::NoOpenPosition);

        let current_mark_price = get_oracle_price(&ctx.accounts.oracle_price_feed_account)?;
        let direction_multiplier = if user_position.is_long { 1 } else { -1 };
        let realized_pnl = (user_position.size as i64)
            .checked_mul((current_mark_price as i64 - user_position.entry_price as i64))
            .ok_or(PerpError::MathOverflow)?
            .checked_mul(direction_multiplier)
            .ok_or(PerpError::MathOverflow)?;

        user_position.unrealized_pnl = realized_pnl;
        let new_collateral = (user_position.collateral as i64)
            .checked_add(realized_pnl)
            .ok_or(PerpError::MathOverflow)?;
        user_position.collateral = if new_collateral < 0 { 0 } else { new_collateral as u64 };

        emit!(PositionClosed {
            user: user_position.user,
            market: user_position.market,
            realized_pnl,
        });

        if user_position.is_long {
            market_state.open_interest_long = market_state
                .open_interest_long
                .checked_sub(user_position.size)
                .unwrap_or_default();
        } else {
            market_state.open_interest_short = market_state
                .open_interest_short
                .checked_sub(user_position.size)
                .unwrap_or_default();
        }

        user_position.size = 0;
        user_position.entry_price = 0;
        user_position.is_long = false;
        user_position.unrealized_pnl = 0;

        Ok(())
    }

    /// Settle funding unchanged.
    pub fn settle_funding(ctx: Context<SettleFunding>) -> Result<()> {
        let market_state = &mut ctx.accounts.market_state;
        let user_position = &mut ctx.accounts.user_position;

        let funding_payment = (user_position.size as i64)
            .checked_mul(market_state.funding_rate)
            .ok_or(PerpError::MathOverflow)?;

        let updated_collateral = (user_position.collateral as i64)
            .checked_add(funding_payment)
            .ok_or(PerpError::MathOverflow)?;
        user_position.collateral = if updated_collateral < 0 { 0 } else { updated_collateral as u64 };

        emit!(FundingSettled {
            user: user_position.user,
            market: user_position.market,
            funding_payment,
        });

        Ok(())
    }
    // The place_stop_order & trigger_stop_order functions will remain unchanged or serve as an alternative.  
    // The bracket order offers a more advanced approach, while both options can coexist.  

}

// =======================================
// HELPERS & INTERNAL LOGIC
// =======================================

/// Checks margin, factoring in dynamic margin and basic volatility.
fn is_margin_healthy(
    user_position: &UserPosition,
    market_state: &MarketState,
    _maybe_mark_price: Option<u64>,
) -> (bool, i64) {
    let current_mark_price = 1000; // placeholder
    let direction_multiplier = if user_position.is_long { 1 } else { -1 };

    let unrealized_pnl = (user_position.size as i64)
        .checked_mul((current_mark_price as i64 - user_position.entry_price as i64))
        .unwrap_or_default()
        .checked_mul(direction_multiplier)
        .unwrap_or_default();

    let net_equity = (user_position.collateral as i64)
        .checked_add(unrealized_pnl)
        .unwrap_or_default();

    // Dynamic margin logic from base_margin_ratio_bps + size factor.
    let dynamic_add = (user_position.size / 10) as u64;
    let dynamic_margin_bps = market_state.base_margin_ratio_bps + dynamic_add;

    // A basic 'volatility' check can also be implemented.  
    // For demonstration purposes, this implementation does not fetch data from oracles.  
    // If base_asset_symbol == "SOL", the required margin is doubled.  
    // This is a placeholder.  
    let mut final_margin_bps = dynamic_margin_bps;
    if market_state.base_asset_symbol == "SOL" {
        final_margin_bps = final_margin_bps.saturating_mul(2);
    }

    let mmr = (user_position.collateral as i64)
        .checked_mul(final_margin_bps as i64)
        .unwrap_or_default()
        .checked_div(1000)
        .unwrap_or_default();

    (net_equity >= mmr, net_equity)
}

fn handle_auto_deleveraging(market_state: &mut MarketState) -> Result<()> {
    msg!("Auto-deleverage check: placeholder. In production, forcibly reduce large winning positions.");
    Ok(())
}

/// Oracle price fetch placeholder.
fn get_oracle_price(oracle_account: &AccountInfo) -> Result<u64> {
    // Updated to use pyth-sdk-solana v0.8.0
    let clock_ts_i64 = Clock::get()?.unix_timestamp;
    // Convert i64 -> u64 safely (returning error on negative)
    let clock_ts_u64 = u64::try_from(clock_ts_i64).map_err(|_| error!(PerpError::MathOverflow))?;

    let price_feed = load_price_feed_from_account_info(oracle_account)
        .map_err(|_| error!(PerpError::InvalidAmount))?;

    //  allow up to 60 seconds of staleness, for example.
    let max_staleness = 60;
    let price_data = price_feed
        .get_price_no_older_than(max_staleness, clock_ts_u64)
        .ok_or(PerpError::InvalidAmount)?;

    // If price is negative, consider it invalid.
    if price_data.price < 0 {
        return Err(error!(PerpError::MathOverflow));
    }

    Ok(price_data.price as u64)
}



// =======================================
// CONTEXTS & ACCOUNTS
// =======================================

#[derive(Accounts)]
#[instruction(initial_funding_rate: i64, base_asset_symbol: String, quote_asset_mint: Pubkey)]
pub struct InitializeMarket<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(init, payer = authority, space = 8 + MarketState::MAX_SIZE)]
    pub market_state: Account<'info, MarketState>,

    /// CHECK: Placeholder vault for fees
    #[account(init, payer = authority, space = 8 + 165)]
    pub fee_vault: AccountInfo<'info>,

    /// CHECK: Placeholder vault for insurance fund
    #[account(init, payer = authority, space = 8 + 165)]
    pub insurance_vault: AccountInfo<'info>,

    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct DepositCollateral<'info> {
    #[account(mut)]
    pub user: Signer<'info>,

    #[account(mut)]
    pub market_state: Account<'info, MarketState>,

    #[account(
        constraint = market_state.quote_asset_mint == quote_asset_mint.key() @ PerpError::InvalidMint
    )]
    pub quote_asset_mint: Account<'info, Mint>,

    #[account(
        init_if_needed,
        payer = user,
        space = 8 + UserPosition::MAX_SIZE,
        seeds = [
            b"user_position",
            user.key().as_ref(),
            market_state.key().as_ref()
        ],
        bump
    )]
    pub user_position: Account<'info, UserPosition>,

    #[account(mut)]
    pub user_collateral_account: Account<'info, TokenAccount>,

    #[account(
        init_if_needed,
        payer = user,
        token::mint = quote_asset_mint,
        token::authority = user_vault_authority,
        seeds = [
            b"user_vault",
            user.key().as_ref(),
            market_state.key().as_ref()
        ],
        bump
    )]
    pub user_vault: Account<'info, TokenAccount>,

    /// CHECK:
    #[account(
        seeds = [
            b"user_vault",
            user.key().as_ref(),
            market_state.key().as_ref()
        ],
        bump
    )]
    pub user_vault_authority: AccountInfo<'info>,

    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct WithdrawCollateral<'info> {
    #[account(mut)]
    pub user: Signer<'info>,

    #[account(mut)]
    pub market_state: Account<'info, MarketState>,

    #[account(
        constraint = market_state.quote_asset_mint == quote_asset_mint.key() @ PerpError::InvalidMint
    )]
    pub quote_asset_mint: Account<'info, Mint>,

    #[account(
        mut,
        has_one = user @ PerpError::Unauthorized
    )]
    pub user_position: Account<'info, UserPosition>,

    /// CHECK:
    #[account(
        seeds = [
            b"user_vault",
            user_position.user.as_ref(),
            market_state.key().as_ref()
        ],
        bump,
    )]
    pub user_vault_authority: AccountInfo<'info>,

    #[account(
        mut,
        constraint = user_vault.mint == quote_asset_mint.key() @ PerpError::InvalidMint
    )]
    pub user_vault: Account<'info, TokenAccount>,

    #[account(mut)]
    pub user_collateral_account: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct OpenPosition<'info> {
    #[account(mut)]
    pub user: Signer<'info>,

    #[account(mut)]
    pub market_state: Account<'info, MarketState>,

    #[account(
        mut,
        has_one = user @ PerpError::Unauthorized,
    )]
    pub user_position: Account<'info, UserPosition>,

    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct ClosePosition<'info> {
    #[account(mut)]
    pub user: Signer<'info>,

    #[account(mut)]
    pub market_state: Account<'info, MarketState>,

    #[account(mut, has_one = user @ PerpError::Unauthorized)]
    pub user_position: Account<'info, UserPosition>,

    /// CHECK:
    pub oracle_price_feed_account: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct LiquidatePosition<'info> {
    #[account(mut)]
    pub liquidator: Signer<'info>,

    #[account(mut)]
    pub market_state: Account<'info, MarketState>,

    #[account(mut)]
    pub user_position: Account<'info, UserPosition>,

    /// CHECK:
    pub oracle_price_feed_account: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct UpdateFundingRate<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(mut)]
    pub market_state: Account<'info, MarketState>,

    /// CHECK:
    pub oracle_price_feed_account: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct SettleFunding<'info> {
    #[account(mut)]
    pub market_state: Account<'info, MarketState>,

    #[account(mut)]
    pub user_position: Account<'info, UserPosition>,
}

#[derive(Accounts)]
pub struct PlaceBracketOrder<'info> {
    #[account(mut)]
    pub user: Signer<'info>,

    #[account(mut)]
    pub market_state: Account<'info, MarketState>,

    #[account(
        mut,
        has_one = user @ PerpError::Unauthorized,
    )]
    pub user_position: Account<'info, UserPosition>,

    #[account(init, payer = user, space = 8 + BracketOrder::MAX_SIZE)]
    pub bracket_order: Account<'info, BracketOrder>,

    /// CHECK:
    pub oracle_price_feed_account: AccountInfo<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct TriggerBracketOrder<'info> {
    #[account(mut)]
    pub market_state: Account<'info, MarketState>,

    #[account(mut)]
    pub user_position: Account<'info, UserPosition>,

    #[account(mut)]
    pub bracket_order: Account<'info, BracketOrder>,

    /// CHECK:
    pub oracle_price_feed_account: AccountInfo<'info>,

    #[account(mut)]
    pub user: Signer<'info>,
}

// For backward-compat with original place_stop_order, trigger_stop_order.
// can skip them or keep them if needed.

// =======================================
// ACCOUNT DATA STRUCTS
// =======================================

#[account]
pub struct MarketState {
    pub authority: Pubkey,
    pub base_asset_symbol: String,
    pub quote_asset_mint: Pubkey,

    // Funding
    pub funding_rate: i64,
    pub last_funding_time: i64,

    // Maintenance margin ratio in bps
    pub maintenance_margin_ratio_bps: u64,
    pub base_margin_ratio_bps: u64,
    pub auto_deleverage_enabled: bool,

    pub fee_vault: Pubkey,
    pub insurance_vault: Pubkey,

    pub open_interest_long: u64,
    pub open_interest_short: u64,
    pub index_price: u64,

    // Dutch auction discount
    pub dutch_auction_discount_bps: u64,
}

impl MarketState {
    pub const MAX_SIZE: usize =
        32 + // authority
        (4 + 10) + // base_asset_symbol
        32 + // quote_asset_mint
        8 +  // funding_rate
        8 +  // last_funding_time
        8 +  // maintenance_margin_ratio_bps
        8 +  // base_margin_ratio_bps
        1 +  // auto_deleverage_enabled
        32 + // fee_vault
        32 + // insurance_vault
        8 +  // open_interest_long
        8 +  // open_interest_short
        8 +  // index_price
        8;   // dutch_auction_discount_bps
}

#[account]
pub struct UserPosition {
    pub user: Pubkey,
    pub market: Pubkey,
    pub collateral: u64,
    pub size: u64,
    pub is_long: bool,
    pub entry_price: u64,
    pub unrealized_pnl: i64,
}

impl UserPosition {
    pub const MAX_SIZE: usize =
        32 +  // user
        32 +  // market
        8 +   // collateral
        8 +   // size
        1 +   // is_long
        8 +   // entry_price
        8;    // unrealized_pnl
}

/// Bracket order struct for OCO: stop_loss and take_profit.
#[account]
pub struct BracketOrder {
    pub user: Pubkey,
    pub market: Pubkey,
    pub stop_loss_price: u64,
    pub take_profit_price: u64,
    pub size: u64,
    pub is_long: bool,
}

impl BracketOrder {
    pub const MAX_SIZE: usize =
        32 + // user
        32 + // market
        8 +  // stop_loss_price
        8 +  // take_profit_price
        8 +  // size
        1;   // is_long
}

#[account]
pub struct StopOrder {
    pub user: Pubkey,
    pub market: Pubkey,
    pub trigger_price: u64,
    pub is_take_profit: bool,
    pub size: u64,
    pub is_long: bool,
}

impl StopOrder {
    pub const MAX_SIZE: usize =
        32 +  // user
        32 +  // market
        8 +   // trigger_price
        1 +   // is_take_profit
        8 +   // size
        1;    // is_long
}

// =======================================
// EVENTS
// =======================================
#[event]
pub struct PositionOpened {
    pub user: Pubkey,
    pub market: Pubkey,
    pub size: u64,
    pub is_long: bool,
}

#[event]
pub struct PositionClosed {
    pub user: Pubkey,
    pub market: Pubkey,
    pub realized_pnl: i64,
}

#[event]
pub struct PositionLiquidated {
    pub user: Pubkey,
    pub market: Pubkey,
    pub penalty: i64,
    pub liquidation_size: u64,
}

#[event]
pub struct FundingRateUpdated {
    pub market: Pubkey,
    pub new_funding_rate: i64,
}

#[event]
pub struct FundingSettled {
    pub user: Pubkey,
    pub market: Pubkey,
    pub funding_payment: i64,
}

#[event]
pub struct CollateralDeposited {
    pub user: Pubkey,
    pub amount: u64,
}

#[event]
pub struct CollateralWithdrawn {
    pub user: Pubkey,
    pub amount: u64,
}

#[event]
pub struct StopOrderPlaced {
    pub user: Pubkey,
    pub market: Pubkey,
    pub trigger_price: u64,
    pub is_take_profit: bool,
}

#[event]
pub struct StopOrderTriggered {
    pub user: Pubkey,
    pub market: Pubkey,
}

 // Bracket order events can be added if needed.  


// =======================================
// ERRORS
// =======================================
#[error_code]
pub enum PerpError {
    #[msg("Invalid amount provided.")]
    InvalidAmount,

    #[msg("Unauthorized.")]
    Unauthorized,

    #[msg("Position not liquidatable.")]
    PositionNotLiquidatable,

    #[msg("Math overflow or underflow.")]
    MathOverflow,

    #[msg("Cannot open position in opposite direction.")]
    OppositePositionNotSupported,

    #[msg("No open position.")]
    NoOpenPosition,

    #[msg("Insufficient margin.")]
    InsufficientMargin,

    #[msg("Insufficient collateral to withdraw.")]
    InsufficientCollateral,

    #[msg("Trigger condition not met.")]
    OrderTriggerConditionNotMet,

    #[msg("Invalid mint.")]
    InvalidMint,
}
