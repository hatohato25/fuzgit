//! git 情報へのアクセス層。
//!
//! 読み取りは `gix`（[`read`] / [`repo`]）、書き込みとプレビュー用の色付き出力生成は
//! システムの `git` コマンドへのシェルアウト（[`exec`]）という役割分担を取る。

pub mod exec;
pub mod read;
pub mod repo;
