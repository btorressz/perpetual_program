describe("Perpetual Program Tests", () => {

  const TOKEN_PROGRAM_ID = new web3.PublicKey("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

  let marketStateKp, feeVaultKp, insuranceVaultKp;
  let userPositionKp, userVaultKp;
  let quoteAssetMint;

  before(async () => {
    // Generate keypairs for the MarketState, Vaults, and User
    marketStateKp = web3.Keypair.generate();
    feeVaultKp = web3.Keypair.generate();
    insuranceVaultKp = web3.Keypair.generate();
    userPositionKp = web3.Keypair.generate();
    userVaultKp = web3.Keypair.generate();

    // Placeholder for USDC/SOL mint
    quoteAssetMint = web3.Keypair.generate().publicKey;
  });

  it("Initializes Market", async () => {
    const initialFundingRate = new BN(0);
    const baseAssetSymbol = "SOL";

    const txHash = await pg.program.methods
      .initializeMarket(initialFundingRate, baseAssetSymbol, quoteAssetMint)
      .accounts({
        marketState: marketStateKp.publicKey,
        feeVault: feeVaultKp.publicKey,
        insuranceVault: insuranceVaultKp.publicKey,
        authority: pg.wallet.publicKey,
        systemProgram: web3.SystemProgram.programId,
        tokenProgram: TOKEN_PROGRAM_ID, 
      })
      .signers([marketStateKp, feeVaultKp, insuranceVaultKp])
      .rpc();

    console.log(`InitializeMarket txHash: ${txHash}`);
    await pg.connection.confirmTransaction(txHash);

    const marketState = await pg.program.account.marketState.fetch(marketStateKp.publicKey);
    console.log("MarketState on-chain:", marketState);

    assert.strictEqual(marketState.authority.toBase58(), pg.wallet.publicKey.toBase58());
    assert.strictEqual(marketState.baseAssetSymbol, "SOL");
    assert.strictEqual(marketState.quoteAssetMint.toBase58(), quoteAssetMint.toBase58());
  });

  it("Deposits Collateral", async () => {
    const depositAmount = new BN(1000);

    const txHash = await pg.program.methods
      .depositCollateral(depositAmount)
      .accounts({
        user: pg.wallet.publicKey,
        marketState: marketStateKp.publicKey,
        quoteAssetMint,
        userPosition: userPositionKp.publicKey,
        userCollateralAccount: userVaultKp.publicKey,
        userVault: userVaultKp.publicKey,
        userVaultAuthority: pg.wallet.publicKey,
        systemProgram: web3.SystemProgram.programId,
        tokenProgram: TOKEN_PROGRAM_ID, // Fix applied here
      })
      .signers([userPositionKp, userVaultKp])
      .rpc();

    console.log(`DepositCollateral txHash: ${txHash}`);
    await pg.connection.confirmTransaction(txHash);

    const userPosition = await pg.program.account.userPosition.fetch(userPositionKp.publicKey);
    console.log("UserPosition after deposit:", userPosition);
    assert(userPosition.collateral.eq(depositAmount), "Collateral not updated correctly");
  });

  it("Opens a Position", async () => {
    const isLong = true;
    const positionSize = new BN(1);

    const txHash = await pg.program.methods
      .openPosition(isLong, positionSize)
      .accounts({
        user: pg.wallet.publicKey,
        marketState: marketStateKp.publicKey,
        userPosition: userPositionKp.publicKey,
        systemProgram: web3.SystemProgram.programId,
        tokenProgram: TOKEN_PROGRAM_ID, // Fix applied here
      })
      .rpc();

    console.log(`OpenPosition txHash: ${txHash}`);
    await pg.connection.confirmTransaction(txHash);

    const userPosition = await pg.program.account.userPosition.fetch(userPositionKp.publicKey);
    console.log("UserPosition after openPosition:", userPosition);
    assert.strictEqual(userPosition.isLong, isLong, "Position direction mismatch");
    assert(userPosition.size.eq(positionSize), "Position size mismatch");
  });

  it("Closes a Position", async () => {
    const txHash = await pg.program.methods
      .closePosition()
      .accounts({
        user: pg.wallet.publicKey,
        marketState: marketStateKp.publicKey,
        userPosition: userPositionKp.publicKey,
        oraclePriceFeedAccount: marketStateKp.publicKey,
      })
      .rpc();

    console.log(`ClosePosition txHash: ${txHash}`);
    await pg.connection.confirmTransaction(txHash);

    const userPosition = await pg.program.account.userPosition.fetch(userPositionKp.publicKey);
    console.log("UserPosition after closePosition:", userPosition);
    assert.strictEqual(userPosition.size.toNumber(), 0, "Size should be zero after closing");
  });

  it("Updates Funding Rate", async () => {
    const txHash = await pg.program.methods
      .updateFundingRate()
      .accounts({
        authority: pg.wallet.publicKey,
        marketState: marketStateKp.publicKey,
        oraclePriceFeedAccount: marketStateKp.publicKey,
      })
      .rpc();

    console.log(`UpdateFundingRate txHash: ${txHash}`);
    await pg.connection.confirmTransaction(txHash);

    const marketState = await pg.program.account.marketState.fetch(marketStateKp.publicKey);
    console.log("MarketState after updateFundingRate:", marketState);
  });

  it("Liquidates a Position", async () => {
    await pg.program.methods
      .openPosition(true, new BN(1))
      .accounts({
        user: pg.wallet.publicKey,
        marketState: marketStateKp.publicKey,
        userPosition: userPositionKp.publicKey,
        systemProgram: web3.SystemProgram.programId,
        tokenProgram: TOKEN_PROGRAM_ID, 
      })
      .rpc();

    const liquidationSize = new BN(1);
    const txHash = await pg.program.methods
      .liquidatePosition(liquidationSize)
      .accounts({
        liquidator: pg.wallet.publicKey,
        marketState: marketStateKp.publicKey,
        userPosition: userPositionKp.publicKey,
        oraclePriceFeedAccount: marketStateKp.publicKey,
      })
      .rpc();

    console.log(`LiquidatePosition txHash: ${txHash}`);
    await pg.connection.confirmTransaction(txHash);

    const userPosition = await pg.program.account.userPosition.fetch(userPositionKp.publicKey);
    console.log("UserPosition after liquidation:", userPosition);
  });
});
