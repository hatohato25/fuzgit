//! git 情報へのアクセス層。
//!
//! 読み取りは `gix`（[`read`] / [`repo`] / [`siblings`]）、書き込みとプレビュー用の
//! 色付き出力生成はシステムの `git` コマンドへのシェルアウト（[`exec`]）という役割分担を取る。

pub mod exec;
pub mod read;
pub mod repo;
pub mod siblings;
