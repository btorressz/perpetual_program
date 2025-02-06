# perpetual_program (Perpetual Trading Protocol on Solana)



## Overview

- This program implements an on-chain perpetual futures/options trading protocol on Solana. It is designed for high-frequency trading (HFT) and liquidations, featuring multi-asset collateral support, bracket orders, dynamic funding rates, and automated liquidations. The program leverages Anchor Framework for smart contract(Solana program) development and Pyth Oracles for real-time price feeds.

- This program was developed in Solana Playground IDE

devnet:(https://explorer.solana.com/address/6QZ2P8VX7ENknVJJ4Tgm5ZbVAzCiL6daW349FhTG8PW7?cluster=devnet)


## 🚀 Features

**🔹 Multi-Asset Collateral Support**

- Users can deposit SOL, USDC, or other SPL tokens as collateral.

- Ensures margin health before allowing withdrawals.

**🔹 Order Types: OCO & Bracket Orders**

- Supports stop-loss and take-profit orders.

- Enables high-frequency trading (HFT) strategies.

**🔹 Adaptive Funding Rate**

- Adjusts funding rate dynamically based on open interest (OI) imbalance.

- Reduces risks associated with prolonged imbalances.

**🔹 Liquidation Automation**

- Allows anyone to liquidate positions, but optimized for automated keepers.

- Uses Dutch auction-style liquidation discounts to encourage participation.

## 🔹 Smart Leverage Limits

- Prevents excessive leverage based on volatility and market conditions.

- Ensures long-term solvency of the protocol.

