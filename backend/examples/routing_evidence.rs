// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Offline held-out aggregation. No AppState, network, credentials or writes to DB.
use std::{fs::{File,OpenOptions},io::{Read,Write},path::Path};
use fosnie_backend::model_routing::{ModelProfile,learning::{self,EvidenceBatch}};

fn read<T:serde::de::DeserializeOwned>(path:&Path)->anyhow::Result<T> {
    let mut bytes=Vec::new();File::open(path)?.take(4_194_305).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len()<=4_194_304,"input exceeds 4 MiB");
    Ok(serde_json::from_slice(&bytes)?)
}
fn main()->anyhow::Result<()> {
    let args:Vec<_>=std::env::args_os().skip(1).collect();
    anyhow::ensure!(args.len()==1 || args.len()==3,"Usage: routing_evidence SAVED_PROFILE.json [BATCH.json NEW_OUTPUT.json]");
    let profile:ModelProfile=read(Path::new(&args[0]))?;
    if args.len()==1 {println!("{}",learning::fingerprint(&profile));return Ok(());}
    let batch:EvidenceBatch=read(Path::new(&args[1]))?;
    let record=learning::aggregate(&profile,batch)?;
    let data=serde_json::to_vec_pretty(&vec![record])?;
    // Refuse to overwrite a prior report or any supplied input.
    let mut file=OpenOptions::new().write(true).create_new(true).open(&args[2])?;
    file.write_all(&data)?;file.write_all(b"\n")?;
    println!("Created unreviewed evidence; source and measured quality are unchanged.");Ok(())
}
