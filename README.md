# MINER

Proof-of-work mined SPL token on Solana. Every MINER you hold adds +1% mining
power. Fixed emission of 10 tokens/min split pro-rata among miners each round.

Website & web miner: [miner.tools](https://miner.tools)

## Mainnet addresses

| What | Address |
|---|---|
| Program | `FyTBuifdJ1u3rF2bsK2NmjzogkCbNK3KtFfZyM3CUfv1` |
| Mint ($MINER) | `GNuooA9WSTDazufDHksrdkspCieoxBERuWNUewkMbyzG` |
| Treasury PDA (mint authority) | `7dPJNrgvYr1zSro9kztPhcjNN8ZE5z4cHYRJohQJtjkd` |
| Squads multisig vault (admin + upgrade authority, 2-of-3, 2h timelock) | `2eiusJRgWkTrNUBau6wCFYLUYXWHhCYhUsofPVFJ6EWj` |

The mint authority is a program PDA — nobody can mint outside the algorithm.
Admin can only tune bounded parameters (difficulty ≤ 32 bits, round length
10 s – 1 h, base weight > 0), gated by a 2-of-3 multisig with a 2-hour
timelock.

## Mine from the CLI

```bash
cargo install miner-cli --locked

# defaults: RPC_URL=mainnet (public), KEYPAIR=~/.config/solana/id.json
miner status   # protocol + your miner state
miner mine     # mining loop (registers on first run)
miner claim    # mint pending rewards to your wallet
miner crank    # optional: run the permissionless round crank
```

## How it works

- Time is split into rounds (`config.round_seconds`, currently 60 s). Each
  round freezes a budget of 10 tokens/min pro-rata. Empty round = emission
  lapses.
- One PoW submit (keccak, proof-of-liveness) per miner per round — extra
  submits are rejected, so faster infrastructure gives no edge.
- Weight = base (100 tokens) + `min(balance now, balance at previous submit)`
  — each token adds +1% power over the base; fresh tokens count from the next
  round (anti-cycling / anti-flash).
- Rewards settle lazily on the next submit/claim; `claim` mints from the
  treasury PDA.
- A session key can sign submits (background mining from the browser) but
  never claims.
- Round accounts are closable after retention (rent goes to the closer);
  unsettled rewards from closed rounds lapse.

## Repository layout

- `api/` — constants, account layouts (Config/Round/Miner), instruction
  definitions, PDAs, instruction builders ([crates.io](https://crates.io/crates/miner-api))
- `program/` — on-chain instruction processors
- `program/tests/` — LiteSVM integration tests (happy paths + attacks:
  cycling, flash balance, duplicates, authorization)
- `cli/` — terminal miner ([crates.io](https://crates.io/crates/miner-cli), binary: `miner`)

## Build & test

```bash
cargo build-sbf     # compile the program to SBF (requires Agave / solana CLI)
cargo test          # LiteSVM integration tests (needs build-sbf first)
```

## License

MIT
