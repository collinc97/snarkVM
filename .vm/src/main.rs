// Copyright (C) 2019-2022 Aleo Systems Inc.
// This file is part of the snarkVM library.

// The snarkVM library is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// The snarkVM library is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with the snarkVM library. If not, see <https://www.gnu.org/licenses/>.

use console::{
    account::{Address, ComputeKey, PrivateKey, ViewKey},
    network::{prelude::*, Network},
    transition::{Record, State},
    types::{Field, Group, Scalar, U64},
};
use snarkvm_vm::{input, output, proof, Transition};

use core::panic::{RefUnwindSafe, UnwindSafe};
use rand::prelude::ThreadRng;
use std::time::Instant;

pub struct Transaction<N: Network> {
    /// The network ID.
    network: u16,
    /// The ordered list of transitions in this transaction.
    transitions: Vec<Transition<N>>,
}

impl<N: Network> Transaction<N> {
    /// Returns `true` if the transition is valid.
    pub fn verify(&self) -> bool {
        // Ensure the network ID matches.
        if self.network != N::ID {
            eprintln!("Network ID mismatch: expected {}, found {}", N::ID, self.network);
            return false;
        }

        // Ensure there is at least one transition.
        if self.transitions.is_empty() {
            eprintln!("No transitions found");
            return false;
        }
        // Ensure the number of transitions is less than the maximum.
        else if self.transitions.len() > N::MAX_TRANSITIONS {
            eprintln!("Exceed maximum transitions: expected {}, found {}", N::MAX_TRANSITIONS, self.transitions.len());
            return false;
        }
        // Ensure the transitions are valid.
        else if self.transitions.iter().any(|transition| !transition.verify()) {
            eprintln!("Invalid transition");
            return false;
        }

        true
    }

    /// Returns the transitions in the transaction.
    pub fn transitions(&self) -> &Vec<Transition<N>> {
        &self.transitions
    }
}

/// Returns the re-randomized balance commitment as `bcm := Commit(balance, r_bcm + r_bcm')`.
fn bcm<A: circuit::Aleo>(
    balance: U64<A::Network>,
    record_view_key: Field<A::Network>,
) -> Result<(Group<A::Network>, Scalar<A::Network>)> {
    // Compute the randomizer for the balance commitment (i.e. HashToScalar(G^r^view_key)).
    let mut r_bcm = A::Network::hash_to_scalar_psd2(&[A::Network::bcm_domain(), record_view_key])?;
    // Compute the re-randomizer for the balance commitment (i.e. HashToScalar(G^r^view_key)).
    r_bcm += A::Network::hash_to_scalar_psd2(&[A::Network::r_bcm_domain(), record_view_key])?;
    // Compute the re-randomized balance commitment (i.e. Commit(balance, r_bcm + r_bcm')).
    let bcm = A::Network::commit_ped64(&balance.to_bits_le(), &r_bcm)?;
    // Return the re-randomized balance commitment.
    Ok((bcm, r_bcm))
}

/// Returns the fee commitment `fcm` and fee randomizer `r_fcm`, where:
///   - `fcm := Σ bcm_in - Σ bcm_out - Commit(fee, 0) = Commit(0, r_fcm)`
///   - `r_fcm := Σ r_in - Σ r_out`.
fn fcm<A: circuit::Aleo>(
    r_in: &[Scalar<A::Network>],
    r_out: &[Scalar<A::Network>],
) -> Result<(Group<A::Network>, Scalar<A::Network>)> {
    // Compute the fee randomizer.
    let mut r_fcm = Scalar::<A::Network>::zero();
    r_in.iter().for_each(|r| r_fcm += r);
    r_out.iter().for_each(|r| r_fcm -= r);
    // Compute the fee commitment.
    let fcm = A::Network::commit_ped64(&0u64.to_bits_le(), &r_fcm)?;
    Ok((fcm, r_fcm))
}

/// Returns the transition view key commitment as `tcm := Hash(caller, tpk, tvk)`.
#[allow(clippy::type_complexity)]
fn tcm<A: circuit::Aleo, R: Rng + CryptoRng>(
    caller: &Address<A::Network>,
    rng: &mut R,
) -> Result<(Field<A::Network>, Group<A::Network>, Field<A::Network>, Group<A::Network>)> {
    // Sample a random nonce.
    let r_tcm = Uniform::rand(rng);
    // Compute the transition secret key `tsk` as `HashToScalar(r_tcm)`.
    // TODO (howardwu): Domain separator.
    // let tsk = A::Network::hash_to_scalar_psd2(&[A::Network::tvk_domain(), r_tcm])?;
    let tsk = A::Network::hash_to_scalar_psd2(&[r_tcm])?;
    // Compute the transition public key `tpk` as `tsk * G`.
    let tpk = A::Network::g_scalar_multiply(&tsk);
    // Compute the transition view key `tvk` as `tsk * caller`.
    let tvk = **caller * tsk;
    // Compute the transition view key commitment `tcm` := `Hash(tvk)`.
    // TODO (howardwu): Domain separator.
    // Compute the transition view key commitment `tcm` as `Hash(caller, tpk, tvk)`.
    let tcm = A::Network::hash_psd4(&[**caller, tpk, tvk].map(|c| c.to_x_coordinate()))?;
    Ok((tcm, tpk, r_tcm, tvk))
}

/// Transition: 0 -> 1
fn mint<A: circuit::Aleo, R: Rng + CryptoRng>(
    caller: &Address<A::Network>,
    amount: u64,
    rng: &mut R,
) -> Result<Transaction<A::Network>>
where
    A::BaseField: UnwindSafe + RefUnwindSafe,
    A::ScalarField: UnwindSafe + RefUnwindSafe,
    A::Network: UnwindSafe + RefUnwindSafe,
    <A::Network as Environment>::Projective: UnwindSafe + RefUnwindSafe,
    A::Affine: UnwindSafe + RefUnwindSafe,
{
    // Set the output index to 0.
    let output_index = 0u16;

    // Compute the transition view key commitment.
    let (tcm, tpk, r_tcm, tvk) = tcm::<A, R>(caller, rng)?;

    // Compute the encryption randomizer as `HashToScalar(tvk || index)`.
    let randomizer = A::Network::hash_to_scalar_psd2(&[tvk.to_x_coordinate(), Field::from_u16(output_index)])?;

    // Initialize th empty data.
    let data = Field::zero(); // TODO: Hardcode this option in the Network trait.
    // Compute the program state nonce.
    let nonce = A::Network::g_scalar_multiply(&randomizer);
    // Initialize a coinbase.
    let state = State::new(*caller, amount, data, nonce);

    // Encrypt the state into a record.
    let record = state.encrypt(&randomizer)?;

    // Compute the record view key as `randomizer * address`.
    let record_view_key = (**caller * randomizer).to_x_coordinate();
    // Compute the randomizer for the balance commitment (i.e. HashToScalar(G^r^view_key));
    let r_bcm = A::Network::hash_to_scalar_psd2(&[A::Network::bcm_domain(), record_view_key])?;
    // Compute the fee commitment.
    let (fcm, r_fcm) = fcm::<A>(&[], &[r_bcm])?;

    let process = std::panic::catch_unwind(|| {
        let public = output::circuit::Public::<A>::from(output_index, record.clone(), fcm, tcm, tpk);
        let private = output::circuit::Private::<A>::from(*caller, state, r_fcm, r_tcm);
        output::circuit::OutputCircuit::from(public, private)?.execute();
        println!("Is satisfied? {} ({} constraints)", A::is_satisfied(), A::num_constraints());

        let (num_constant, num_public, num_private, num_constraints, num_gates) = A::count();
        println!(
            "Count(Constant: {num_constant}, Public: {num_public}, Private: {num_private}, Constraints: {num_constraints}, Gates: {num_gates})"
        );

        let timer = Instant::now();
        let assignment = circuit::Circuit::eject();
        println!("Convert to assignment: {} ms", timer.elapsed().as_millis());

        let proof = proof::snark::execute(assignment)?;
        let transition = Transition {
            program: Field::<A::Network>::zero(), // TODO: Hardcode this option in the Network trait.
            process: Field::<A::Network>::zero(), // TODO: Hardcode this option in the Network trait.
            inputs: vec![],
            outputs: vec![output::Output::new(record)],
            input_proofs: vec![],
            output_proofs: vec![proof],
            tcm,
            tpk,
            fee: -(amount as i64),
        };
        assert_eq!(fcm, transition.fcm()?);

        let transaction = Transaction { network: A::Network::ID, transitions: vec![transition] };
        assert!(transaction.verify());

        Ok::<_, Error>(transaction)
    });

    match process {
        Ok(Ok(transaction)) => Ok(transaction),
        Ok(Err(error)) => bail!("{:?}", error),
        Err(_) => bail!("Thread failed"),
    }
}

/// Transition: 1 -> 0
fn burn<A: circuit::Aleo, R: Rng + CryptoRng>(
    caller_private_key: &PrivateKey<A::Network>,
    record: &Record<A::Network>,
    rng: &mut R,
) -> Result<Transaction<A::Network>>
where
    A::BaseField: UnwindSafe + RefUnwindSafe,
    A::ScalarField: UnwindSafe + RefUnwindSafe,
    A::Affine: UnwindSafe + RefUnwindSafe,
    <A::Network as Environment>::Projective: UnwindSafe + RefUnwindSafe,
    A::Network: UnwindSafe + RefUnwindSafe,
{
    // Initialize the caller compute key, view key, and address.
    let caller_compute_key = ComputeKey::try_from(caller_private_key)?;
    let caller_view_key = ViewKey::try_from(caller_private_key)?;
    let caller_address = Address::try_from(caller_private_key)?;

    // Compute the record commitment.
    let commitment = record.to_commitment()?;

    // Initialize a program tree with the coinbase record.
    let program = A::Network::merkle_tree_bhp::<32>(&[commitment.to_bits_le()])?; // TODO: Add test that record ID matches in tree.
    // Compute a Merkle path for the coinbase record.
    let merkle_path = program.prove(0, &commitment.to_bits_le())?;
    // Retrieve the Merkle root.
    let root = program.root();

    // Compute the record view key.
    let record_view_key = record.to_record_view_key(&caller_view_key);

    // Compute the serial number and signature.
    // TODO (howardwu): Add the *serial_number.serial_number() to the message.
    let serial_number =
        record.to_serial_number(&caller_private_key.sk_sig(), &caller_compute_key.pr_sig(), &[], rng)?;

    // Decrypt the record into program state.
    let state = record.decrypt_symmetric(&record_view_key)?;
    let fee = *state.balance() as i64;

    // Compute the balance commitment.
    let (bcm, r_bcm) = bcm::<A>(state.balance(), record_view_key)?;
    // Compute the fee commitment.
    let (fcm, r_fcm) = fcm::<A>(&[r_bcm], &[])?;
    // Compute the transition view key commitment.
    let (tcm, tpk, r_tcm, _tvk) = tcm::<A, R>(&caller_address, rng)?;

    let process = std::panic::catch_unwind(|| {
        let public = input::circuit::Public::<A>::from(*root, *serial_number.value(), bcm, fcm, tcm, tpk);
        let private = input::circuit::Private::<A>::from(
            record_view_key,
            record.clone(),
            merkle_path,
            serial_number.clone(),
            r_fcm,
            r_tcm,
        );
        let input_circuit = input::circuit::InputCircuit::from(public, private)?;
        input_circuit.execute();

        let (num_constant, num_public, num_private, num_constraints, num_gates) = A::count();
        println!(
            "Count(Constant: {num_constant}, Public: {num_public}, Private: {num_private}, Constraints: {num_constraints}, Gates: {num_gates})"
        );

        let timer = Instant::now();
        let assignment = circuit::Circuit::eject();
        println!("Convert to assignment: {} ms", timer.elapsed().as_millis());

        let proof = proof::snark::execute(assignment)?;
        let transition = Transition {
            program: Field::<A::Network>::zero(), // TODO: Hardcode this option in the Network trait.
            process: Field::<A::Network>::zero(), // TODO: Hardcode this option in the Network trait.
            inputs: vec![input::Input::new(*serial_number.value(), bcm)],
            outputs: vec![],
            input_proofs: vec![proof],
            output_proofs: vec![],
            tcm,
            tpk,
            fee,
        };
        assert_eq!(fcm, transition.fcm()?);

        let transaction = Transaction { network: A::Network::ID, transitions: vec![transition] };
        assert!(transaction.verify());

        Ok::<_, Error>(transaction)
    });

    match process {
        Ok(Ok(transaction)) => Ok(transaction),
        Ok(Err(error)) => bail!("{:?}", error),
        Err(_) => bail!("Thread failed"),
    }
}

fn main() -> Result<()> {
    let mut rng = rand::thread_rng();

    // Initialize a new caller account.
    let caller_private_key = PrivateKey::<<circuit::AleoV0 as circuit::Environment>::Network>::new(&mut rng)?;
    let _caller_view_key = ViewKey::try_from(&caller_private_key)?;
    let caller_address = Address::try_from(&caller_private_key)?;

    // Generate a coinbase transaction.
    let transaction = mint::<circuit::AleoV0, ThreadRng>(&caller_address, 100u64, &mut rng)?;

    // Retrieve the coinbase record.
    let record = transaction.transitions()[0].outputs[0].record();

    // Spend the coinbase record.
    let _transaction = burn::<circuit::AleoV0, ThreadRng>(&caller_private_key, record, &mut rng)?;

    Ok(())
}
