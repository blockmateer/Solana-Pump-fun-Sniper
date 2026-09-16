# Solana Pumpfun Sniper Bot 
This bot fetches to new pumpfun pools and buys as soon as possible. If RPC or node is good, it commonly buy tokens before token is availabel on Pumpfun UI, can buy tokens than the others. It's free, basic version, and I have advanced version for it. I updated codebase with Rust to boost speed and performance. Feel free to contact with me to get advanced version. [Telegram: https://t.me/solana_ghost_trader, Whatsapp: https://wa.me/13137423660]. This is just version to give vision about pumpfun and sniper bot.



## Features
### Speed and Efficiency
- **Lightning-Quick Transactions**: Leveraging Rust's exceptional performance, our bot allows you to snipe new tokens almost instantly. Say goodbye to delays and seize opportunities as they arise!
### Safety First
- **Robust Security**: Rust's safety guarantees minimize bugs and vulnerabilities, ensuring your trading activities are secure. Trade with confidence and peace of mind.
### Multiple gRPC Connections
- **Stay Updated**: Effortlessly connect to top Solana data providers like **Helius** and **Yellowstone** through multiple gRPC connections. Get real-time updates and make informed trading decisions.
### User-Friendly Interface
- **Intuitive Design**: My sniper bot features a clean and accessible interface, making it easy for users of all experience levels to navigate. Start trading in no time!



## Advanced Features
- **jito-confirm**: Engage in low-latency transactions on platforms like Raydium and Pumpfun.
- **jito-bundle**: Bundle buy/sell actions with up to **20 wallets** in Raydium/Pumpfun, enhancing your trading strategy and flexibility.



---

## Directory Structure

```
src/
├── core/
│   ├── token.rs        # Token definitions and handling
│   └── tx.rs           # Transaction handling
|   └── mod.rs          # mod file
| 
├── engine/
│   ├── swap.rs         # Token swap(buy/sell) functionalities in various Dexs
│   └── monitor         # New token monitoring(and parse tx) in Dexs using geyser rpc, and normal rpc
│       └── helius.rs               # Helius gRpc for tx listen and parse.
│       └── yellowstone.rs          # Yellowstone gRpc for tx listen and parse.
|   └── mod.rs          # mod file
|
├── dex/
│   ├── pumpfun.rs      # Pumpfun
|   └── mod.rs          # mod file
│
├── services/
│   ├── jito.rs        # Jito service provides ultra-fast transaction confirmation
│   ├── nozomi.rs        # Jito service provides ultra-fast transaction confirmation
│   ├── zeroslot.rs        # Jito service provides ultra-fast transaction confirmation
│   └── nextblock.rs        # NextBlock service provides the ultra-fast transaction confirmation in unique way
|   └── mod.rs          # mod file
|
├── common/
│   ├── logger.rs        # Logs to be clean and convenient to monitor.
│   └── utils.rs        # Utility functions used across the project
|   └── mod.rs          # mod file
│
├── lib.rs
└── main.rs
```
---



### Setup
1. Install [Rust](https://rustup.rs/) (stable).
2. Copy `.env.example` to `.env` and fill in your values:
```plaintext
PRIVATE_KEY= # your wallet priv_key (base58)
RPC_HTTPS= # Helius / premium HTTPS RPC
RPC_WSS= # matching WebSocket RPC
SLIPPAGE=10
JITO_BLOCK_ENGINE_URL=https://ny.mainnet.block-engine.jito.wtf
JITO_TIP_VALUE=0.0001
TOKEN_AMOUNT=0.01 # SOL amount to buy per new token
UNIT_PRICE=20000
UNIT_LIMIT=300000
```
3. Add wallet/mint addresses to block (one per line) in `trial/blacklist.txt`.
4. On Windows, install [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) with the **Desktop development with C++** workload (needed for `link.exe`).
5. Build and run:
```bash
cargo build --release
cargo run --release
```
Or run the prebuilt trial binary if present: `trial/pumpfun-sniper.exe`.



### Test Result(Same Block)
- Detect: https://solscan.io/tx/5o7ajnZ9CRf7FBYEvydu8vapJJDWtKCvRFiTUBmbeu2FmmDhAQQy3c9YFFhpTucr2SZcrf2aUsDanEVjYgwN9kBc
- Bought: https://solscan.io/tx/3vgim3MwJsdtahXqfW2DrzTAWpVQ8EUTed2cjzHuqxSfUpfp72mgzZhiVosWaCUHdqJTDHpQaYh5xN7rkHGmzqWv
- Dexscreener: https://dexscreener.com/solana/A1zZXCq2DmqwVD4fLDzmgQ3ceY6LQnMBVokejqnHpump


### Contact Information
- Telegram: https://t.me/DevCutup
- Whatsapp: https://wa.me/13137423660
- Twitter: https://x.com/devcutup
