//! fuzgit — fuzzy finder で「選ぶ」「探す」「辿る」git 操作 CLI（実行コマンド名 `gz`）。
//!
//! バイナリ（`src/main.rs`）は薄いエントリポイントに留め、実装はこのライブラリ側に置く。
//! こうすることで統合テストからも同じ API を利用できる。

pub mod cli;
pub mod commands;
pub mod error;
pub mod finder;
pub mod git;

#[cfg(test)]
mod test_support;
