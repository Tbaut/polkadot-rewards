// Copyright 2021 Parity Technologies (UK) Ltd.
// This file is part of polkadot-rewards.

// polkadot-rewards is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// polkadot-rewards is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with polkadot-rewards.  If not, see <http://www.gnu.org/licenses/>.

use crate::{api::Api, primitives::CsvRecord};
use anyhow::{anyhow, bail, Context, Error};
use argh::FromArgs;
use chrono::{naive::NaiveDateTime, offset::Utc};
use env_logger::{Builder, Env};
use indicatif::{ProgressBar, ProgressStyle};
use std::{fs::File, io, path::PathBuf, str::FromStr};

const OUTPUT_DATE: &str = "%Y-%m-%d";

#[derive(FromArgs, PartialEq, Debug)]
/// Polkadot Staking Rewards CLI-App
pub struct App {
	#[argh(option, from_str_fn(date_from_string), short = 'f')]
	/// date to start crawling for staking rewards. Format: "YYY-MM-DD HH:MM:SS"
	pub from: NaiveDateTime,
	/// date to stop crawling for staking rewards. Defaults to current time. Format: "YYY-MM-DD HH:MM:SS"
	#[argh(option, from_str_fn(date_from_string), default = "default_date()", short = 't')]
	pub to: NaiveDateTime,
	/// network to crawl for rewards. One of: [Polkadot, Kusama, KSM, DOT]
	#[argh(option, default = "Network::Polkadot", short = 'n')]
	pub network: Network,
	/// the fiat currency which should be used for prices
	#[argh(option, short = 'c')]
	pub currency: String,
	/// network-formatted address to get staking rewards for.
	#[argh(option, short = 'a')]
	pub address: String,
	/// date format to use in output CSV data. Uses rfc2822 by default.  EX: "%Y-%m-%d %H:%M:%S".
	#[argh(option, default = "OUTPUT_DATE.to_string()")]
	date_format: String,
	/// directory to output completed CSV to.
	#[argh(option, default = "default_file_location()", short = 'p')]
	folder: PathBuf,
	/// output the CSV file to STDOUT. Disables creating a new file.
	#[argh(switch, short = 's')]
	stdout: bool,
	/// get extra information about the program's execution.
	#[argh(switch, short = 'v')]
	verbose: bool,
}

fn default_date() -> NaiveDateTime {
	Utc::now().naive_utc()
}

fn default_file_location() -> PathBuf {
	match std::env::current_dir() {
		Err(e) => {
			log::error!("{}", e.to_string());
			std::process::exit(1);
		}
		Ok(p) => p,
	}
}

// we don't return an anyhow::Error here because `argh` macro expects error type to be a `String`
pub fn date_from_string(value: &str) -> Result<chrono::NaiveDateTime, String> {
	let time = match NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S") {
		Ok(t) => Ok(t),
		Err(e) => Err(e.to_string()),
	};
	let time = time?;
	Ok(time)
}

#[derive(PartialEq, Debug)]
pub enum Network {
	/// The Polkadot Network
	Polkadot,
	/// The Kusama Network
	Kusama,
}

impl Network {
	pub fn id(&self) -> &'static str {
		match self {
			Self::Polkadot => "polkdadot",
			Self::Kusama => "kusama",
		}
	}
}

impl FromStr for Network {
	type Err = Error;
	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s.to_lowercase().as_str() {
			"polkadot" | "dot" => Ok(Network::Polkadot),
			"kusama" | "ksm" => Ok(Network::Kusama),
			_ => bail!("Network must be one of: 'kusama', 'polkadot', 'dot', 'ksm'"),
		}
	}
}

pub fn app() -> Result<(), Error> {
	let mut app: App = argh::from_env();
	let progress = if app.verbose {
		Builder::from_env(Env::default().default_filter_or("info")).init();
		None
	} else {
		Some(construct_progress_bar())
	};
	let api = Api::new(&app, progress.as_ref());

	let rewards = api
		.fetch_all_rewards(app.from.timestamp() as usize, app.to.timestamp() as usize)
		.context("Failed to fetch rewards.")?;
	let prices = api.fetch_prices(&rewards).context("Failed to fetch prices.")?;

	let file_name = construct_file_name(&app);
	app.folder.push(&file_name);
	app.folder.set_extension("csv");

	let mut wtr = Output::new(&app).context("Failed to create output.")?;

	for (reward, price) in rewards.iter().zip(prices.iter()) {
		wtr.serialize(CsvRecord {
			block_num: reward.block_num,
			block_time: reward.day.format(&app.date_format).to_string(),
			amount: amount_to_network(&app.network, &reward.amount),
			price: *price.market_data.current_price.get(&app.currency).ok_or_else(|| {
				anyhow!(
					"Specified fiat currency '{}' not supported: {:#?}",
					app.currency,
					price.market_data.current_price.keys(),
				)
			})?,
		})
		.context("Failed to format CsvRecord")?;
	}

	if app.stdout {
		progress.map(|p| p.finish_with_message("Writing data to STDOUT"));
	} else {
		progress.map(|p| p.finish_with_message(&format!("wrote data to file {}", &file_name)));
	}
	Ok(())
}

fn construct_progress_bar() -> ProgressBar {
	let bar = ProgressBar::new(1000);
	bar.set_style(
		ProgressStyle::default_bar()
			.template("{spinner:.blue} {msg} [{elapsed_precise}] [{bar:40.cyan/blue}] {percent}% ({eta})")
			.progress_chars("#>-"),
	);
	bar
}

fn amount_to_network(network: &Network, amount: &u128) -> f64 {
	match network {
		Network::Polkadot => *amount as f64 / (10000000000f64),
		Network::Kusama => *amount as f64 / (1000000000000f64),
	}
}

// constructs a file name in the format: `dot-address-from_date-to_date-rewards.csv`
fn construct_file_name(app: &App) -> String {
	format!(
		"{}-{}-{}-{}-rewards",
		app.network.id(),
		&app.address,
		app.from.format(OUTPUT_DATE),
		app.to.format(OUTPUT_DATE)
	)
}

enum Output {
	FileOut(csv::Writer<File>),
	StdOut(csv::Writer<std::io::Stdout>),
}

impl Output {
	fn new(app: &App) -> Result<Self, Error> {
		let mut builder = csv::WriterBuilder::new();
		builder.delimiter(b';');
		if app.stdout {
			Ok(Output::StdOut(builder.from_writer(io::stdout())))
		} else {
			let file = File::create(&app.folder)?;
			Ok(Output::FileOut(builder.from_writer(file)))
		}
	}

	fn serialize<T: serde::Serialize>(&mut self, val: T) -> Result<(), Error> {
		match self {
			Output::FileOut(f) => f.serialize(val)?,
			Output::StdOut(s) => s.serialize(val)?,
		};
		Ok(())
	}
}
