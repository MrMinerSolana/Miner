pub mod consts;
pub mod error;
pub mod instruction;
pub mod pda;
pub mod sdk;
pub mod state;

use solana_program::declare_id;

// Mainnet program id. (The devnet deployment runs under a different id:
// GdHXeu5JDYec2Xq4USGeRswHK6gVfcrcjYQ5uJUCbw15.)
declare_id!("FyTBuifdJ1u3rF2bsK2NmjzogkCbNK3KtFfZyM3CUfv1");
