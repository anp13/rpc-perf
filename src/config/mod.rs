// Copyright 2021 Twitter, Inc.
// Licensed under the Apache License, Version 2.0
// http://www.apache.org/licenses/LICENSE-2.0

// use crate::config_file::*;
use rand::Rng;
use rand_distr::Alphanumeric;
use rand_distr::Uniform;
use rand_distr::{Distribution, WeightedAliasIndex};
use rand_xoshiro::Xoshiro256Plus;
use std::net::SocketAddr;
use zipf::ZipfDistribution;

mod file;

pub use file::*;

pub const NAME: &str = env!("CARGO_PKG_NAME");
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone)]
pub struct Config {
    general: General,
    debug: Debug,
    // waterfall: Waterfall,
    connection: Connection,
    request: Request,
    // tls: Option<Tls>,
    endpoints: Vec<SocketAddr>,
    keyspaces: Vec<Keyspace>,
    keyspace_dist: WeightedAliasIndex<usize>,
}

#[derive(Clone)]
pub enum KeyDistribution {
    Uniform(Uniform<usize>),
    Zipf(ZipfDistribution),
}

impl KeyDistribution {
    pub fn sample(&self, rng: &mut Xoshiro256Plus) -> usize {
        match self {
            Self::Uniform(d) => d.sample(rng),
            Self::Zipf(d) => d.sample(rng),
        }
    }
}

#[derive(Clone)]
pub struct Keyspace {
    length: usize,
    weight: usize,
    cardinality: u32,
    commands: Vec<Command>,
    command_dist: WeightedAliasIndex<usize>,
    inner_keys: Vec<InnerKey>,
    inner_key_dist: Option<WeightedAliasIndex<usize>>,
    values: Vec<Value>,
    value_dist: WeightedAliasIndex<usize>,
    // ttl: usize,
    key_type: FieldType,
    // batch_size: usize,
    key_distribution: KeyDistribution,
}

impl Keyspace {
    pub fn length(&self) -> usize {
        self.length
    }

    pub fn cardinality(&self) -> u32 {
        self.cardinality
    }

    // TODO(aetimmes): implement cardinality for Alphanumeric fields
    pub fn generate_key(&self, rng: &mut Xoshiro256Plus) -> Vec<u8> {
        match self.key_type {
            FieldType::Alphanumeric => rng
                .sample_iter(&Alphanumeric)
                .take(self.length())
                .collect::<Vec<u8>>(),
            FieldType::U32 => format!(
                "{:0>len$}",
                self.key_distribution.sample(rng) as u32,
                len = self.length()
            )
            .as_bytes()
            .to_vec(),
        }
    }

    //#TODO(atimmes): implement cardinality for Alphanumeric fields
    pub fn generate_inner_key(&self, rng: &mut Xoshiro256Plus) -> Option<Vec<u8>> {
        if let Some(ref dist) = self.inner_key_dist {
            let idx = dist.sample(rng);
            let conf = &self.inner_keys[idx];
            let inner_key = match conf.field_type() {
                FieldType::Alphanumeric => rng
                    .sample_iter(&Alphanumeric)
                    .take(conf.length())
                    .collect::<Vec<u8>>(),
                FieldType::U32 => format!(
                    "{:0>len$}",
                    &rng.gen_range(0u32..conf.cardinality()),
                    len = conf.length()
                )
                .as_bytes()
                .to_vec(),
            };
            Some(inner_key)
        } else {
            None
        }
    }

    //#TODO(atimmes): implement cardinality for Alphanumeric fields
    pub fn generate_value(&self, rng: &mut Xoshiro256Plus) -> Vec<u8> {
        // if let Some(ref value_dist) = self.value_dist {
        let value_idx = self.value_dist.sample(rng);
        let value_conf = &self.values[value_idx];
        let value = match value_conf.field_type() {
            FieldType::Alphanumeric => rng
                .sample_iter(&Alphanumeric)
                .take(value_conf.length())
                .collect::<Vec<u8>>(),
            FieldType::U32 => format!(
                "{:0>len$}",
                &rng.gen_range(0u32..value_conf.cardinality()),
                len = value_conf.length()
            )
            .as_bytes()
            .to_vec(),
        };
        value
        // } else {
        //     None
        // }
    }

    pub fn choose_command(&self, rng: &mut Xoshiro256Plus) -> &Command {
        &self.commands[self.command_dist.sample(rng)]
    }

    pub fn choose_value(&self, rng: &mut Xoshiro256Plus) -> &Value {
        &self.values[self.value_dist.sample(rng)]
    }

    // pub fn ttl(&self) -> usize {
    //     self.ttl
    // }

    // pub fn batch_size(&self) -> usize {
    //     self.batch_size
    // }
}

impl Config {
    pub fn new(file: Option<&str>) -> Self {
        let config_file = if let Some(file) = file {
            File::load(file)
        } else {
            fatal!("need a config file");
        };

        let mut keyspaces = Vec::new();
        for k in config_file.keyspaces() {
            let inner_keys = k.inner_keys();
            let inner_key_weights: Vec<usize> = if inner_keys.is_empty() {
                Vec::new()
            } else {
                inner_keys.iter().map(|v| v.weight()).collect()
            };
            let inner_key_dist = if inner_keys.is_empty() {
                None
            } else {
                Some(WeightedAliasIndex::new(inner_key_weights).unwrap())
            };

            let command_weights: Vec<usize> = k.commands().iter().map(|v| v.weight()).collect();
            let command_dist = WeightedAliasIndex::new(command_weights).unwrap();

            let values = k.values();
            let value_weights: Vec<usize> = if values.is_empty() {
                Vec::new()
            } else {
                values.iter().map(|v| v.weight()).collect()
            };
            let value_dist = if values.is_empty() {
                panic!("no values configured for keyspace");
            } else {
                WeightedAliasIndex::new(value_weights).unwrap()
            };

            let key_distribution = match k.key_distribution {
                None => KeyDistribution::Uniform(Uniform::new(0, k.cardinality() as usize)),
                Some(ref kd) => match kd.model {
                    KeyDistributionModel::Uniform => {
                        KeyDistribution::Uniform(Uniform::new(0, k.cardinality() as usize))
                    }
                    KeyDistributionModel::Zipf => {
                        let exponent = kd
                            .parameters
                            .get("exponent")
                            .unwrap_or(&"1.0".to_owned())
                            .parse::<f64>()
                            .expect("bad exponent for zipf distribution");
                        KeyDistribution::Zipf(
                            ZipfDistribution::new(k.cardinality() as usize, exponent)
                                .expect("bad zipf config"),
                        )
                    }
                },
            };

            let keyspace = Keyspace {
                length: k.length(),
                weight: k.weight(),
                cardinality: k.cardinality(),
                commands: k.commands(),
                command_dist,
                inner_keys: k.inner_keys(),
                inner_key_dist,
                values: k.values(),
                value_dist,
                // ttl: k.ttl(),
                key_type: k.key_type(),
                // batch_size: k.batch_size(),
                key_distribution,
            };
            keyspaces.push(keyspace);
        }

        let weights: Vec<usize> = keyspaces.iter().map(|k| k.weight).collect();
        let keyspace_dist = WeightedAliasIndex::new(weights).unwrap();

        if config_file.target().endpoints().is_empty() {
            fatal!("no target endpoints configured");
        }

        Self {
            general: config_file.general(),
            debug: config_file.debug(),
            // waterfall: config_file.waterfall(),
            // tls: config_file.tls(),
            connection: config_file.connection(),
            request: config_file.request(),
            endpoints: config_file.target().endpoints(),
            keyspaces,
            keyspace_dist,
        }
    }

    pub fn general(&self) -> &General {
        &self.general
    }

    pub fn debug(&self) -> &Debug {
        &self.debug
    }

    // pub fn waterfall(&self) -> &Waterfall {
    //     &self.waterfall
    // }

    // pub fn tls(&self) -> Option<&Tls> {
    //     self.tls.as_ref()
    // }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn request(&self) -> &Request {
        &self.request
    }

    pub fn endpoints(&self) -> Vec<SocketAddr> {
        self.endpoints.clone()
    }

    pub fn choose_keyspace(&self, rng: &mut Xoshiro256Plus) -> &Keyspace {
        &self.keyspaces[self.keyspace_dist.sample(rng)]
    }
}
