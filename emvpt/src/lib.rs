use pcsc::{Context, Card, Scope, ShareMode, Protocols, MAX_BUFFER_SIZE, MAX_ATR_SIZE};
use hexplay::HexViewBuilder;
use iso7816_tlv::ber::{Tlv, Tag, Value};
use std::collections::HashMap;
use std::str;
use std::convert::TryFrom;
use std::convert::TryInto;
use std::error;
use serde::{Deserialize, Serialize};
use std::fs::{self};
use log::{info, warn, debug, trace};
use rand::prelude::*;
use rand_chacha::ChaCha20Rng;
use rand::{Rng};
use openssl::rsa::{Rsa, Padding};
use openssl::bn::BigNum;
use openssl::sha;
use hex;
use chrono::{NaiveDate, Datelike, Utc};

pub mod bcdutil;

type ApduInterfaceCustomFunction = fn(&[u8]) -> Result<Vec<u8>, ()>;

pub enum ApduInterface<'a> {
    Pcsc(&'a Option<&'a Card>),
    Function(ApduInterfaceCustomFunction)
}


fn send_apdu_raw<'apdu>(interface : &ApduInterface, apdu : &'apdu [u8]) -> Result<Vec<u8>, ()> {
    let mut output : Vec<u8> = Vec::new();

    match interface {
        ApduInterface::Pcsc(card) => {
            let mut apdu_response_buffer = [0; MAX_BUFFER_SIZE];
            output.extend_from_slice(card.unwrap().transmit(apdu, &mut apdu_response_buffer).unwrap());
        },
        ApduInterface::Function(function) => {
            output.extend_from_slice(&function(apdu).unwrap()[..]);
        }
    }

    Ok(output)
}

macro_rules! get_bit {
    ($byte:expr, $bit:expr) => (if $byte & (1 << $bit) != 0 { true } else { false });
}

macro_rules! set_bit {
    ($byte:expr, $bit:expr, $bit_value:expr) => (if $bit_value == true { $byte |= 1 << $bit; } else { $byte &= !(1 << $bit); });
}

#[repr(u8)]
#[derive(Debug, Copy, Clone)]
pub enum CvmCode {
    FailCvmProcessing = 0b0000_0000,
    PlaintextPin = 0b0000_0001,
    EncipheredPinOnline = 0b0000_0010,
    PlaintextPinAndSignature = 0b0000_0011,
    EncipheredPinOffline = 0b0000_0100,
    EncipheredPinOfflineAndSignature = 0b0000_0101,
    Signature = 0b0001_1110,
    NoCvm = 0b0001_1111
}

impl From<CvmCode> for u8 {
    fn from(orig: CvmCode) -> Self {
        match orig {
            CvmCode::FailCvmProcessing => 0b0000_0000,
            CvmCode::PlaintextPin => 0b0000_0001,
            CvmCode::EncipheredPinOnline => 0b0000_0010,
            CvmCode::PlaintextPinAndSignature => 0b0000_0011,
            CvmCode::EncipheredPinOffline => 0b0000_0100,
            CvmCode::EncipheredPinOfflineAndSignature => 0b0000_0101,
            CvmCode::Signature => 0b0001_1110,
            CvmCode::NoCvm => 0b0001_1111
        }
    }
}

impl TryFrom<u8> for CvmCode {
    type Error = &'static str;

    fn try_from(orig: u8) -> Result<Self, Self::Error> {
        match orig {
            0b0000_0000 => Ok(CvmCode::FailCvmProcessing),
            0b0000_0001 => Ok(CvmCode::PlaintextPin),
            0b0000_0010 => Ok(CvmCode::EncipheredPinOnline),
            0b0000_0011 => Ok(CvmCode::PlaintextPinAndSignature),
            0b0000_0100 => Ok(CvmCode::EncipheredPinOffline),
            0b0000_0101 => Ok(CvmCode::EncipheredPinOfflineAndSignature),
            0b0001_1110 => Ok(CvmCode::Signature),
            0b0001_1111 => Ok(CvmCode::NoCvm),
            _ => Err("Unknown code!")
        }
    }
}

#[repr(u8)]
#[derive(Debug, Copy, Clone)]
pub enum CvmConditionCode {
    Always = 0x00,
    UnattendedCash = 0x01,
    NotCashNorPurchaseWithCashback = 0x02,
    CvmSupported = 0x03,
    ManualCash = 0x04,
    PurchaseWithCashback = 0x05,
    IccCurrencyUnderX = 0x06,
    IccCurrencyOverX = 0x07,
    IccCurrencyUnderY = 0x08,
    IccCurrencyOverY = 0x09
}

impl From<CvmConditionCode> for u8 {
    fn from(orig: CvmConditionCode) -> Self {
        match orig {
            CvmConditionCode::Always => 0x00,
            CvmConditionCode::UnattendedCash => 0x01,
            CvmConditionCode::NotCashNorPurchaseWithCashback => 0x02,
            CvmConditionCode::CvmSupported => 0x03,
            CvmConditionCode::ManualCash => 0x04,
            CvmConditionCode::PurchaseWithCashback => 0x05,
            CvmConditionCode::IccCurrencyUnderX => 0x06,
            CvmConditionCode::IccCurrencyOverX => 0x07,
            CvmConditionCode::IccCurrencyUnderY => 0x08,
            CvmConditionCode::IccCurrencyOverY => 0x09
        }
    }
}

impl TryFrom<u8> for CvmConditionCode {
    type Error = &'static str;

    fn try_from(orig: u8) -> Result<Self, Self::Error> {
        match orig {
            0x00 => Ok(CvmConditionCode::Always),
            0x01 => Ok(CvmConditionCode::UnattendedCash),
            0x02 => Ok(CvmConditionCode::NotCashNorPurchaseWithCashback),
            0x03 => Ok(CvmConditionCode::CvmSupported),
            0x04 => Ok(CvmConditionCode::ManualCash),
            0x05 => Ok(CvmConditionCode::PurchaseWithCashback),
            0x06 => Ok(CvmConditionCode::IccCurrencyUnderX),
            0x07 => Ok(CvmConditionCode::IccCurrencyOverX),
            0x08 => Ok(CvmConditionCode::IccCurrencyUnderY),
            0x09 => Ok(CvmConditionCode::IccCurrencyOverY),
            _ => Err("Unknown condition!")
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub struct CvmRule {
    pub amount_x : u32,
    pub amount_y : u32,
    pub fail_if_unsuccessful : bool,
    pub code : CvmCode,
    pub condition : CvmConditionCode
}

impl CvmRule {
    pub fn into_9f34_value(rule : Result<CvmRule, CvmRule>) -> Vec<u8> {
        // EMV Book 4, A4 CVM Results

        let rule_unwrapped = match rule {
            Ok(rule) => rule,
            Err(rule) => rule
        };

        let mut c : u8 = rule_unwrapped.code.try_into().unwrap();
        if ! rule_unwrapped.fail_if_unsuccessful {
            c += 0b0100_0000;
        }


        let mut value : Vec<u8> = Vec::new();
        value.push(c);
        value.push(rule_unwrapped.condition.try_into().unwrap());

        let result : u8 = match rule {
            Ok(rule) => {
                match rule.code {
                    CvmCode::Signature => 0x00, // unknown
                    _ => 0x02 // successful
                }
            },
            Err(_) => 0x01 // failed
        };

        value.push(result);

        debug!("9F34 {:02X?}: {:?}", value, rule_unwrapped);

        return value;
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Capabilities {
    pub sda : bool,
    pub dda : bool,
    pub cda : bool,
    pub plaintext_pin : bool,
    pub enciphered_pin : bool,
    pub terminal_risk_management : bool,
    pub issuer_authentication : bool
}

#[derive(Debug)]
pub struct UsageControl {
    pub domestic_cash_transactions : bool,
    pub international_cash_transactions : bool,
    pub domestic_goods : bool,
    pub international_goods : bool,
    pub domestic_services : bool,
    pub international_services : bool,
    pub atms : bool,
    pub terminals_other_than_atms : bool,
    pub domestic_cashback : bool,
    pub international_cashback : bool
}

#[derive(Debug)]
pub struct Icc {
    pub capabilities : Capabilities,
    pub usage : UsageControl,
    pub cvm_rules : Vec<CvmRule>
}

impl Icc {
    fn new() -> Icc {
        let capabilities = Capabilities {
            sda: false,
            dda: false,
            cda: false,
            plaintext_pin: false,
            enciphered_pin: false,
            terminal_risk_management: false,
            issuer_authentication: false
        };

        let usage = UsageControl {
            domestic_cash_transactions: false,
            international_cash_transactions: false,
            domestic_goods: false,
            international_goods: false,
            domestic_services: false,
            international_services: false,
            atms: false,
            terminals_other_than_atms: false,
            domestic_cashback: false,
            international_cashback: false
        };

        Icc { capabilities : capabilities, usage : usage, cvm_rules : Vec::new() }
    }
}

// TODO: support EMV Book 4, A2 Terminal Capabilities (a.k.a. 9F33)
#[derive(Serialize, Deserialize)]
pub struct Terminal {
    pub use_random : bool,
    pub capabilities : Capabilities,
    pub tvr : TerminalVerificationResults
}

// EMV Book 3, C5 Terminal Verification Results
#[derive(Serialize, Deserialize, Debug, Copy, Clone)]
pub struct TerminalVerificationResults {
    //TVR byte 1
    pub offline_data_authentication_was_not_performed : bool,
    pub sda_failed : bool,
    pub icc_data_missing : bool,
    pub card_appears_on_terminal_exception_file : bool,
    pub dda_failed : bool,
    pub cda_failed : bool,
    //RFU
    //RFU

    // TVR byte 2
    pub icc_and_terminal_have_different_application_versions : bool,
    pub expired_application : bool,
    pub application_not_yet_effective : bool,
    pub requested_service_not_allowed_for_card_product : bool,
    pub new_card : bool,
    //RFU
    //RFU
    //RFU

    //TVR byte 3
    pub cardholder_verification_was_not_successful : bool,
    pub unrecognised_cvm : bool,
    pub pin_try_limit_exceeded : bool,
    pub pin_entry_required_and_pin_pad_not_present_or_not_working : bool,
    pub pin_entry_required_pin_pad_present_but_pin_was_not_entered : bool,
    pub online_pin_entered : bool,
    //RFU
    //RFU

    //TVR byte 4
    pub transaction_exceeds_floor_limit : bool,
    pub lower_consecutive_offline_limit_exceeded : bool,
    pub upper_consecutive_offline_limit_exceeded : bool,
    pub transaction_selected_randomly_for_online_processing : bool,
    pub merchant_forced_transaction_online : bool,
    //RFU
    //RFU
    //RFU

    //TVR byte 5
    pub default_tdol_used : bool,
    pub issuer_authentication_failed : bool,
    pub script_processing_failed_before_final_generate_ac : bool,
    pub script_processing_failed_after_final_generate_ac : bool
    //RFU
    //RFU
    //RFU
    //RFU
}

impl From<TerminalVerificationResults> for Vec<u8> {
    fn from(tvr: TerminalVerificationResults) -> Self {
        let mut b1 : u8 = 0b0000_0000;
        let mut b2 : u8 = 0b0000_0000;
        let mut b3 : u8 = 0b0000_0000;
        let mut b4 : u8 = 0b0000_0000;
        let mut b5 : u8 = 0b0000_0000;

        set_bit!(b1, 7, tvr.offline_data_authentication_was_not_performed);
        set_bit!(b1, 6, tvr.sda_failed);
        set_bit!(b1, 5, tvr.icc_data_missing);
        set_bit!(b1, 4, tvr.card_appears_on_terminal_exception_file);
        set_bit!(b1, 3, tvr.dda_failed);
        set_bit!(b1, 2, tvr.cda_failed);

        set_bit!(b2, 7, tvr.icc_and_terminal_have_different_application_versions);
        set_bit!(b2, 6, tvr.expired_application);
        set_bit!(b2, 5, tvr.application_not_yet_effective);
        set_bit!(b2, 4, tvr.requested_service_not_allowed_for_card_product);
        set_bit!(b2, 3, tvr.new_card);

        set_bit!(b3, 7, tvr.cardholder_verification_was_not_successful);
        set_bit!(b3, 6, tvr.unrecognised_cvm);
        set_bit!(b3, 5, tvr.pin_try_limit_exceeded);
        set_bit!(b3, 4, tvr.pin_entry_required_and_pin_pad_not_present_or_not_working);
        set_bit!(b3, 3, tvr.pin_entry_required_pin_pad_present_but_pin_was_not_entered);
        set_bit!(b3, 2, tvr.online_pin_entered);

        set_bit!(b4, 7, tvr.transaction_exceeds_floor_limit);
        set_bit!(b4, 6, tvr.lower_consecutive_offline_limit_exceeded);
        set_bit!(b4, 5, tvr.upper_consecutive_offline_limit_exceeded);
        set_bit!(b4, 4, tvr.transaction_selected_randomly_for_online_processing);
        set_bit!(b4, 3, tvr.merchant_forced_transaction_online);

        set_bit!(b5, 7, tvr.default_tdol_used);
        set_bit!(b5, 6, tvr.issuer_authentication_failed);
        set_bit!(b5, 5, tvr.script_processing_failed_before_final_generate_ac);
        set_bit!(b5, 4, tvr.script_processing_failed_after_final_generate_ac);

        let mut output : Vec<u8> = Vec::new();
        output.push(b1);
        output.push(b2);
        output.push(b3);
        output.push(b4);
        output.push(b5);

        output
    }
}


#[derive(Serialize, Deserialize)]
pub struct Settings {
    pub terminal : Terminal,
    default_tags : HashMap<String, String>
}

pub struct EmvConnection<'a> {
    tags : HashMap<String, Vec<u8>>,
    ctx : Option<Context>,
    card : Option<Card>,
    pub interface : Option<ApduInterface<'a>>,
    emv_tags : HashMap<String, EmvTag>,
    pub settings : Settings,
    pub icc : Icc
}

pub enum ReaderError {
    ReaderConnectionFailed(String),
    ReaderNotFound,
    CardConnectionFailed(String),
    CardNotFound
}

impl EmvConnection<'_> {
    pub fn new() -> Result<EmvConnection<'static>, String> {
        let emv_tags = serde_yaml::from_str(&fs::read_to_string("../config/emv_tags.yaml").unwrap()).unwrap();
        let settings = serde_yaml::from_str(&fs::read_to_string("../config/settings.yaml").unwrap()).unwrap();

        Ok ( EmvConnection { tags : HashMap::new(), ctx : None, card : None, emv_tags : emv_tags, settings : settings, icc : Icc::new(), interface : None } )
    }

    pub fn print_tags(&self) {
        let mut i = 0;
        for (key, value) in &self.tags {
            i += 1;
            let emv_tag = self.emv_tags.get(key);
            info!("{:02}. tag: {} - {}", i, key, emv_tag.unwrap_or(&EmvTag { tag: key.clone(), name: "Unknown tag".to_string() }).name);
            info!("value: {:02X?} = {}", value, String::from_utf8_lossy(&value).replace(|c: char| !(c.is_ascii_alphanumeric() || c.is_ascii_punctuation()), "."));
        }

    }

    pub fn connect_to_card(&mut self) -> Result<(), ReaderError> {
        if !self.ctx.is_some() {
            self.ctx = match Context::establish(Scope::User) {
                Ok(ctx) => Some(ctx),
                Err(err) => {
                    return Err(ReaderError::ReaderConnectionFailed(format!("Failed to establish context: {}", err)));
                }
            };
        }

        let ctx = self.ctx.as_ref().unwrap();
        let readers_size = match ctx.list_readers_len() {
            Ok(readers_size) => readers_size,
            Err(err) => {
                return Err(ReaderError::ReaderConnectionFailed(format!("Failed to list readers: {}", err)));
            }
        };

        let mut readers_buf = vec![0; readers_size];
        let mut readers = match ctx.list_readers(&mut readers_buf) {
            Ok(readers) => readers,
            Err(err) => {
                return Err(ReaderError::ReaderConnectionFailed(format!("Failed to list readers: {}", err)));
            }
        };

        let reader = match readers.next() {
            Some(reader) => reader,
            None => {
                return Err(ReaderError::ReaderNotFound);
            }
        };

        // Connect to the card.
        self.card = match ctx.connect(reader, ShareMode::Shared, Protocols::ANY) {
            Ok(card) => {
                Some(card)
            },
            Err(pcsc::Error::NoSmartcard) => {
                return Err(ReaderError::CardNotFound);
            },
            Err(err) => {
                return Err(ReaderError::CardConnectionFailed(format!("Could not connect to the card: {}", err)));
            }
        };

        const MAX_NAME_SIZE : usize = 2048;
        let mut names_buffer = [0; MAX_NAME_SIZE];
        let mut atr_buffer = [0; MAX_ATR_SIZE];
        let card_status = self.card.as_ref().unwrap().status2(&mut names_buffer, &mut atr_buffer).unwrap();

        // https://www.eftlab.com/knowledge-base/171-atr-list-full/
        debug!("Card reader: {:?}", reader);
        debug!("Card ATR:\n{}", HexViewBuilder::new(card_status.atr()).finish());
        debug!("Card protocol: {:?}", card_status.protocol2().unwrap());

        Ok(())
    }

    pub fn get_tag_value(&self, tag_name : &str) -> Option<&Vec<u8>> {
        self.tags.get(tag_name)
    }

    pub fn add_tag(&mut self, tag_name : &str, value : Vec<u8>) {
        if tag_name == "80" {
            return;
        }

        let old_tag = self.tags.get(tag_name);
        if old_tag.is_some() {
            trace!("Overriding tag {:?} from {:02X?} to {:02X?}", tag_name, old_tag.unwrap(), value);
        }

        self.tags.insert(tag_name.to_string(), value);
    }

    fn send_apdu_select(&mut self, aid : &[u8]) -> (Vec<u8>, Vec<u8>) {
        self.tags.clear();

        let apdu_command_select = b"\x00\xA4\x04\x00";

        let mut select_command = apdu_command_select.to_vec();
        select_command.push(aid.len() as u8);
        select_command.extend_from_slice(aid);

        self.send_apdu(&select_command)
    }

    fn send_apdu<'apdu>(&mut self, apdu : &'apdu [u8]) -> (Vec<u8>, Vec<u8>) {

        let mut response_data : Vec<u8> = Vec::new();
        let mut response_trailer : Vec<u8>;

        let mut new_apdu_command;
        let mut apdu_command = apdu;


        let card = &self.card.as_ref();
        let mut interface : &ApduInterface = &ApduInterface::Pcsc(card);
        if self.interface.is_some() {
            interface = self.interface.as_ref().unwrap();
        }

        loop {
            // Send an APDU command.
            debug!("Sending APDU:\n{}", HexViewBuilder::new(&apdu_command).finish());

            let apdu_response = send_apdu_raw(interface, apdu_command).unwrap();

            response_data.extend_from_slice(&apdu_response[0..apdu_response.len()-2]);

            // response codes: https://www.eftlab.com/knowledge-base/complete-list-of-apdu-responses/
            response_trailer = vec![apdu_response[apdu_response.len()-2], apdu_response[apdu_response.len()-1]];
            debug!("APDU response status: {:02X?}", response_trailer);

            // Automatically query more data, if available from the ICC
            const SW1_BYTES_AVAILABLE : u8 = 0x61;
            const SW1_WRONG_LENGTH : u8    = 0x6C;

            if response_trailer[0] == SW1_BYTES_AVAILABLE {
                trace!("APDU response({} bytes):\n{}", response_data.len(), HexViewBuilder::new(&response_data).finish());

                let mut available_data_length = response_trailer[1];

                // NOTE: EMV doesn't have a use case where ICC would pass bigger records than what is passable with a single ADPU response
                if available_data_length == 0x00 {
                    // there are more than 255 bytes available, query the maximum
                    available_data_length = 0xFF;
                }

                let apdu_command_get_response = b"\x00\xC0\x00\x00";
                new_apdu_command = apdu_command_get_response.to_vec();
                new_apdu_command.push(available_data_length);

                apdu_command = &new_apdu_command[..];
            } else if response_trailer[0] == SW1_WRONG_LENGTH {
                trace!("APDU response({} bytes):\n{}", response_data.len(), HexViewBuilder::new(&response_data).finish());

                let available_data_length = response_trailer[1];
                assert!(available_data_length > 0x00);

                new_apdu_command = apdu.to_vec();
                let new_apdu_command_length = new_apdu_command.len();
                new_apdu_command[new_apdu_command_length - 1] = available_data_length;

                apdu_command = &new_apdu_command[..];
            } else {
                break;
            }
        }

        debug!("APDU response({} bytes):\n{}", response_data.len(), HexViewBuilder::new(&response_data).finish());

        if !response_data.is_empty() {
            debug!("APDU TLV parse:");

            self.process_tlv(&response_data[..], 0);
        }

        (response_trailer, response_data)
    }

    fn print_tag(emv_tag : &EmvTag, level: u8) {
        let mut padding = String::with_capacity(level as usize);
        for _ in 0..level {
            padding.push(' ');
        }
        debug!("{}-{}: {}", padding, emv_tag.tag, emv_tag.name);
    }
    fn print_tag_value(v : &Vec<u8>, level: u8) {
        let mut padding = String::with_capacity(level as usize);
        for _ in 0..level {
            padding.push(' ');
        }
        debug!("{}-data: {:02X?} = {}", padding, v, String::from_utf8_lossy(&v).replace(|c: char| !(c.is_ascii_alphanumeric() || c.is_ascii_punctuation()), "."));

    }

    pub fn process_tlv(&mut self, buf: &[u8], level: u8) {
        let mut read_buffer = buf;

        loop {
            let (tlv_data, leftover_buffer) = Tlv::parse(read_buffer);

            let tlv_data : Tlv = match tlv_data {
                Ok(tlv) => tlv,
                Err(err) => {
                    if leftover_buffer.len() > 0 {
                        trace!("Could not parse as TLV! error:{:?}, data: {:02X?}", err, read_buffer);
                    }

                    break;
                }

            };

            read_buffer = leftover_buffer;


            let tag_name = hex::encode(tlv_data.tag().to_bytes()).to_uppercase();

            match self.emv_tags.get(tag_name.as_str()) {
                Some(emv_tag) => {
                    EmvConnection::print_tag(&emv_tag, level);
                },
                _ => {
                    let unknown_tag = EmvTag { tag: tag_name.clone(), name: "Unknown tag".to_string() };
                    EmvConnection::print_tag(&unknown_tag, level);
                }
            }

            match tlv_data.value() {
                Value::Constructed(v) => {
                    for tlv_tag in v {
                        self.process_tlv(&tlv_tag.to_vec(), level + 1);
                    }
                },
                Value::Primitive(v) => {
                    self.add_tag(&tag_name, v.to_vec());

                    EmvConnection::print_tag_value(v, level);
                }
            };

            if leftover_buffer.len() == 0 {
                break;
            }
        }
    }

    pub fn handle_get_processing_options(&mut self) -> Result<Vec<u8>, ()> {
        debug!("GET PROCESSING OPTIONS:");
        let get_processing_options_command = b"\x80\xA8\x00\x00\x02\x83\x00".to_vec();
        let (response_trailer, response_data) = self.send_apdu(&get_processing_options_command);
        if !is_success_response(&response_trailer) {
            warn!("Could not get processing options");
            return Err(());
        }

        if response_data[0] == 0x80 {
            self.add_tag("82", response_data[2..4].to_vec());
            self.add_tag("94", response_data[4..].to_vec());
        } else if response_data[0] != 0x77 {
            warn!("Unrecognized response");
            return Err(());
        }

        let tag_94_afl = self.get_tag_value("94").unwrap().clone();

        debug!("Read card AFL information:");

        let mut data_authentication : Vec<u8> = Vec::new();
        assert_eq!(tag_94_afl.len() % 4, 0);
        let mut records : Vec<u8> = Vec::new();
        for i in (0..tag_94_afl.len()).step_by(4) {
            let short_file_identifier : u8 = tag_94_afl[i] >> 3;
            let record_index_start : u8 = tag_94_afl[i+1];
            let record_index_end : u8 = tag_94_afl[i+2];
            let mut data_authentication_records : u8 = tag_94_afl[i+3];

            for record_index in record_index_start..record_index_end+1 {
                if let Some(data) = self.read_record(short_file_identifier, record_index) {
                    assert_eq!(data[0], 0x70);
                    records.extend(&data);

                    // Add data authentication input            
                    // ref EMV Book 3, 10.3 Offline Data Authentication
                    if data_authentication_records > 0 {
                        data_authentication_records -= 1;

                        if short_file_identifier <= 10 {
                            if let Value::Constructed(tag_70_tags) = parse_tlv(&data[..]).unwrap().value() {
                                for tag in tag_70_tags {
                                    data_authentication.extend(tag.to_vec());
                                }
                            }
                        } else {
                            data_authentication.extend_from_slice(&data[..]);
                        }
                        
                        trace!("Data authentication building: short_file_identifier:{}, data_authentication_records:{}, record_index:{}/{}, data:{:02X?}", short_file_identifier, data_authentication_records, record_index, record_index_end, data_authentication);
                    }
                }
            }
        }

        debug!("AFL data authentication:\n{}", HexViewBuilder::new(&data_authentication).finish());

        let tag_82_aip = self.get_tag_value("82").unwrap();

        let auc_b1 : u8 = tag_82_aip[0];
        // bit 7 = RFU
        self.icc.capabilities.sda = get_bit!(auc_b1, 6);
        self.icc.capabilities.dda = get_bit!(auc_b1, 5);
        if get_bit!(auc_b1, 4) {
            // Cardholder verification is supported

            let tag_8e_cvm_list = self.get_tag_value("8E").unwrap().clone();
            let amount1 = &tag_8e_cvm_list[0..4];
            let amount2 = &tag_8e_cvm_list[4..8];
            let amount_x = str::from_utf8(&bcdutil::bcd_to_ascii(&amount1[..]).unwrap()[..]).unwrap().parse::<u32>().unwrap();
            let amount_y = str::from_utf8(&bcdutil::bcd_to_ascii(&amount2[..]).unwrap()[..]).unwrap().parse::<u32>().unwrap();

            let tag_84_cvm_rules = &tag_8e_cvm_list[8..];
            assert_eq!(tag_84_cvm_rules.len() % 2, 0);
            for i in (0..tag_84_cvm_rules.len()).step_by(2) {
                let cvm_rule = &tag_84_cvm_rules[i..i+2];
                let cvm_code = cvm_rule[0];
                let cvm_condition_code = cvm_rule[1];

                // bit 7 = RFU
                let fail_if_unsuccessful = ! get_bit!(cvm_code, 6);
                let cvm_code = (cvm_code << 2) >> 2;
                let code : CvmCode = cvm_code.try_into().unwrap();
                let condition : CvmConditionCode = cvm_condition_code.try_into().unwrap();

                let rule = CvmRule { amount_x : amount_x, amount_y : amount_y, fail_if_unsuccessful : fail_if_unsuccessful, code : code, condition : condition };
                self.icc.cvm_rules.push(rule);
            }
        }
        self.icc.capabilities.terminal_risk_management = get_bit!(auc_b1, 3);
        // Issuer Authentication using the EXTERNAL AUTHENTICATE command is supported
        self.icc.capabilities.issuer_authentication = get_bit!(auc_b1, 2);
        // bit 1 = RFU
        self.icc.capabilities.cda = get_bit!(auc_b1, 0);

        let tag_9f07_application_usage_control = self.get_tag_value("9F07").unwrap();
        let auc_b1 : u8 = tag_9f07_application_usage_control[0];
        let auc_b2 : u8 = tag_9f07_application_usage_control[1];
        self.icc.usage.domestic_cash_transactions = get_bit!(auc_b1, 7);
        self.icc.usage.international_cash_transactions = get_bit!(auc_b1, 6);
        self.icc.usage.domestic_goods = get_bit!(auc_b1, 5);
        self.icc.usage.international_goods = get_bit!(auc_b1, 4);
        self.icc.usage.domestic_services = get_bit!(auc_b1, 3);
        self.icc.usage.international_services = get_bit!(auc_b1, 2);
        self.icc.usage.atms = get_bit!(auc_b1, 1);
        self.icc.usage.terminals_other_than_atms = get_bit!(auc_b1, 0);
        self.icc.usage.domestic_cashback = get_bit!(auc_b2, 7);
        self.icc.usage.international_cashback = get_bit!(auc_b2, 6);

        debug!("{:?}", self.icc);

        // 5 - 0 bits are RFU

        Ok(data_authentication)
    }

    pub fn handle_verify_plaintext_pin(&mut self, ascii_pin : &[u8]) -> Result<(), ()> {
        debug!("Verify plaintext PIN:");

        let pin_bcd_cn = bcdutil::ascii_to_bcd_cn(ascii_pin, 6).unwrap();

        let apdu_command_verify = b"\x00\x20\x00";
        let mut verify_command = apdu_command_verify.to_vec();
        let p2_pin_type_qualifier = 0b1000_0000;
        verify_command.push(p2_pin_type_qualifier);
        verify_command.push(0x08); // data length
        verify_command.push(0b0010_0000 + ascii_pin.len() as u8); // control + PIN length
        verify_command.extend_from_slice(&pin_bcd_cn[..]);
        verify_command.push(0xFF); // filler

        let (response_trailer, _) = self.send_apdu(&verify_command);
        if !is_success_response(&response_trailer) {
            warn!("Could not verify PIN");
            //Incorrect PIN = 63, C4
            return Err(());
        }

        info!("Pin OK");
        Ok(())
    }

    fn fill_random(&self, data : &mut [u8]) {
        if self.settings.terminal.use_random {
            let mut rng = ChaCha20Rng::from_entropy();
            rng.try_fill(data).unwrap();
        }
    }

    pub fn handle_verify_enciphered_pin(&mut self, ascii_pin : &[u8], icc_pin_pk_modulus : &[u8], icc_pin_pk_exponent : &[u8]) -> Result<(), ()> {
        debug!("Verify enciphered PIN:");

        let pin_bcd_cn = bcdutil::ascii_to_bcd_cn(ascii_pin, 6).unwrap();

        const PK_MAX_SIZE : usize = 248; // ref. EMV Book 2, B2.1 RSA Algorithm
        let mut random_padding = [0u8; PK_MAX_SIZE];
        self.fill_random(&mut random_padding[..]);

        let icc_unpredictable_number = self.handle_get_challenge().unwrap();

        // EMV Book 2, 7.1 Keys and Certificates, 7.2 PIN Encipherment and Verification

        let mut plaintext_data = Vec::new();
        plaintext_data.push(0x7F);
        // PIN block
        plaintext_data.push(0b0010_0000 + ascii_pin.len() as u8); // control + PIN length
        plaintext_data.extend_from_slice(&pin_bcd_cn[..]);
        plaintext_data.push(0xFF);
        // ICC Unpredictable Number
        plaintext_data.extend_from_slice(&icc_unpredictable_number[..]);
        // Random padding
        plaintext_data.extend_from_slice(&random_padding[0..icc_pin_pk_modulus.len()-17]);

        let icc_pin_pk = RsaPublicKey::new(icc_pin_pk_modulus, icc_pin_pk_exponent);
        let ciphered_pin_data = icc_pin_pk.public_encrypt(&plaintext_data[..]).unwrap();

        let apdu_command_verify = b"\x00\x20\x00";
        let mut verify_command = apdu_command_verify.to_vec();
        let p2_pin_type_qualifier = 0b1000_1000;
        verify_command.push(p2_pin_type_qualifier);
        verify_command.push(ciphered_pin_data.len() as u8);
        verify_command.extend_from_slice(&ciphered_pin_data[..]);

        let (response_trailer, _) = self.send_apdu(&verify_command);
        if !is_success_response(&response_trailer) {
            warn!("Could not verify PIN");
            //Incorrect PIN = 63, C4
            return Err(());
        }

        info!("Pin OK");
        Ok(())
    }

    pub fn handle_generate_ac(&mut self) -> Result<(), ()> {
        debug!("Generate Application Cryptogram (GENERATE AC):");

        let tag_95_tvr : Vec<u8> = self.settings.terminal.tvr.into();
        self.add_tag("95", tag_95_tvr);
        debug!("{:?}", self.settings.terminal.tvr);

        let cdol_data = self.get_tag_list_tag_values(&self.get_tag_value("8C").unwrap()[..]).unwrap();
        assert!(cdol_data.len() <= 0xFF);

        let p1_tc_proceed_offline = 0b0100_0000;

        let apdu_command_generate_ac = b"\x80\xAE";
        let mut generate_ac_command = apdu_command_generate_ac.to_vec();
        generate_ac_command.push(p1_tc_proceed_offline);
        generate_ac_command.push(0x00);
        generate_ac_command.push(cdol_data.len() as u8);
        generate_ac_command.extend_from_slice(&cdol_data);
        generate_ac_command.push(0x00);

        let (response_trailer, response_data) = self.send_apdu(&generate_ac_command);
        if !is_success_response(&response_trailer) {
            // 67 00 = wrong length (i.e. CDOL data incorrect)
            warn!("Could not process generate ac");
            return Err(());
        }

        if response_data[0] == 0x80 {
            self.add_tag("9F27", response_data[2..3].to_vec());
            self.add_tag("9F36", response_data[3..5].to_vec());
            self.add_tag("9F26", response_data[5..13].to_vec());
            if response_data.len() > 13 {
                self.add_tag("9F10", response_data[13..].to_vec());
            } 
        } else if response_data[0] != 0x77 {
            warn!("Unrecognized response");
            return Err(());
        }

        //let tag_9f27_cryptogram_information_data = connection.get_tag_value("9F27").unwrap();
        //let tag_9f36_application_transaction_counter = connection.get_tag_value("9F36").unwrap();
        //let tag_9f26_application_cryptogram = connection.get_tag_value("9F26").unwrap();
        //let tag_9f10_issuer_application_data = connection.get_tag_value("9F10");

        Ok(())
    }

    fn read_record(&mut self, short_file_identifier : u8, record_index : u8) -> Option<Vec<u8>> {
        let mut records : Vec<u8> = Vec::new();

        let apdu_command_read = b"\x00\xB2";

        let mut read_record = apdu_command_read.to_vec();
        read_record.push(record_index);
        read_record.push((short_file_identifier << 3) | 0x04);

        const RECORD_LENGTH_DEFAULT : u8 = 0x00;
        read_record.push(RECORD_LENGTH_DEFAULT);

        let (response_trailer, response_data) = self.send_apdu(&read_record);

        if is_success_response(&response_trailer) {
            records.extend_from_slice(&response_data);
        }

        if !records.is_empty() {
            return Some(records);
        }

        None
    }

    pub fn handle_select_payment_system_environment(&mut self) -> Result<Vec<EmvApplication>, ()> {
        debug!("Selecting Payment System Environment (PSE):");
        let contact_pse_name = "1PAY.SYS.DDF01";

        let pse_name = contact_pse_name;

        let (response_trailer, _) = self.send_apdu_select(&pse_name.as_bytes());
        if !is_success_response(&response_trailer) {
            warn!("Could not select {:?}", pse_name);
            return Err(());
        }

        let sfi_data = self.get_tag_value("88").unwrap().clone();
        assert_eq!(sfi_data.len(), 1);
        let short_file_identifier = sfi_data[0];

        debug!("Read available AIDs:");

        let mut all_applications : Vec<EmvApplication> = Vec::new();

        for record_index in 0x01..0xFF {
            match self.read_record(short_file_identifier, record_index) {
                Some(data) => {
                    if data[0] != 0x70 {
                        warn!("Expected template data");
                        return Err(());
                    }

                    if let Value::Constructed(application_templates) = parse_tlv(&data).unwrap().value() {
                        for tag_61_application_template in application_templates {
                            if let Value::Constructed(application_template) = tag_61_application_template.value() {
                                self.tags.clear();

                                for application_template_child_tag in application_template {
                                    if let Value::Primitive(value) = application_template_child_tag.value() {
                                        let tag_name = hex::encode(application_template_child_tag.tag().to_bytes()).to_uppercase();
                                        self.add_tag(&tag_name, value.to_vec());
                                    }
                                }

                                let tag_4f_aid = self.get_tag_value("4F").unwrap();
                                let tag_50_label = self.get_tag_value("50").unwrap();
                                
                                if let Some(tag_87_priority) = self.get_tag_value("87") {
                                    all_applications.push(EmvApplication {
                                        aid: tag_4f_aid.clone(),
                                        label: tag_50_label.clone(),
                                        priority: tag_87_priority.clone()
                                    });
                                } else {
                                    debug!("Skipping application. AID:{:02X?}, label:{:?}", tag_4f_aid, str::from_utf8(&tag_50_label).unwrap());
                                }

                            }
                        }
                    }

                },
                None => break
            };
        }

        if all_applications.is_empty() {
            warn!("No application records found!");
            return Err(());
        }

        Ok(all_applications)
    }

    pub fn handle_select_payment_application(&mut self, application : &EmvApplication) -> Result<(), ()> {
        info!("Selecting application. AID:{:02X?}, label:{:?}, priority:{:02X?}", application.aid, str::from_utf8(&application.label).unwrap(), application.priority);
        let (response_trailer, _) = self.send_apdu_select(&application.aid);
        if !is_success_response(&response_trailer) {
            warn!("Could not select payment application! {:02X?}, {:?}", application.aid, application.label);
            return Err(());
        }

        Ok(())
    }

    pub fn process_settings(&mut self) -> Result<(), Box<dyn error::Error>> {
        let default_tags = self.settings.default_tags.clone();
        for (tag_name, tag_value) in default_tags.iter() {
            self.add_tag(&tag_name, hex::decode(&tag_value.clone())?);
        }

        if !self.get_tag_value("9A").is_some() {
            let today = Utc::today().naive_utc();
            let transaction_date_ascii_yymmdd = format!("{:02}{:02}{:02}",today.year()-2000, today.month(), today.day());
            self.add_tag("9A", bcdutil::ascii_to_bcd_cn(transaction_date_ascii_yymmdd.as_bytes(), 3).unwrap());
        }

        if !self.get_tag_value("9F37").is_some() {
            let mut tag_9f37_unpredictable_number = [0u8; 4];
            self.fill_random(&mut tag_9f37_unpredictable_number[..]);

            self.add_tag("9F37", tag_9f37_unpredictable_number.to_vec());
        }

        Ok(())
    }

    pub fn handle_get_data(&mut self, tag : &[u8]) -> Result<Vec<u8>, ()> {
        debug!("GET DATA:");

        assert_eq!(tag.len(), 2);
        assert_eq!(tag[0], 0x9F);
        //allowed tags: 9F36, 9F13, 9F17 or 9F4F

        let apdu_command_get_data = b"\x80\xCA";

        let mut get_data_command = apdu_command_get_data.to_vec();
        get_data_command.extend_from_slice(tag);
        get_data_command.push(0x05);

        let (response_trailer, response_data) = self.send_apdu(&get_data_command[..]);
        if !is_success_response(&response_trailer) {
            // 67 00 = wrong length (i.e. CDOL data incorrect)
            warn!("Could not process get data");
            return Err(());
        }

        let mut output : Vec<u8> = Vec::new();
        output.extend_from_slice(&response_data);

        Ok(output)
    }

    pub fn handle_get_challenge(&mut self) -> Result<Vec<u8>, ()> {
        debug!("GET CHALLENGE:");

        let apdu_command_get_challenge = b"\x00\x84\x00\x00\x00";

        let (response_trailer, response_data) = self.send_apdu(&apdu_command_get_challenge[..]);
        if !is_success_response(&response_trailer) {
            // 67 00 = wrong length (i.e. CDOL data incorrect)
            warn!("Could not process get challenge");
            return Err(());
        }

        let mut output : Vec<u8> = Vec::new();
        output.extend_from_slice(&response_data);

        Ok(output)
    }

    pub fn get_issuer_public_key(&self, application : &EmvApplication) -> Result<(Vec<u8>, Vec<u8>), ()> {

        // ref. https://www.emvco.com/wp-content/uploads/2017/05/EMV_v4.3_Book_2_Security_and_Key_Management_20120607061923900.pdf - 6.3 Retrieval of Issuer Public Key
        let ca_data : HashMap<String, CertificateAuthority> = serde_yaml::from_str(&fs::read_to_string("../config/scheme_ca_public_keys.yaml").unwrap()).unwrap();


        let tag_92_issuer_pk_remainder = self.get_tag_value("92").unwrap();
        let tag_9f32_issuer_pk_exponent = self.get_tag_value("9F32").unwrap();
        let tag_90_issuer_public_key_certificate = self.get_tag_value("90").unwrap();

        let rid = &application.aid[0..5];
        let tag_8f_ca_pk_index = self.get_tag_value("8F").unwrap();
     
        let ca_pk = get_ca_public_key(&ca_data, &rid[..], &tag_8f_ca_pk_index[..]).unwrap();

        let issuer_certificate = ca_pk.public_decrypt(&tag_90_issuer_public_key_certificate[..]).unwrap();
        let issuer_certificate_length = issuer_certificate.len();

        if issuer_certificate[1] != 0x02 {
            warn!("Incorrect issuer certificate type {:02X?}", issuer_certificate[1]);
            return Err(());
        }

        let checksum_position = 15 + issuer_certificate_length - 36;

        let issuer_certificate_iin    = &issuer_certificate[2..6];
        let issuer_certificate_expiry = &issuer_certificate[6..8];
        let issuer_certificate_serial = &issuer_certificate[8..11];
        let issuer_certificate_hash_algorithm = &issuer_certificate[11..12];
        let issuer_pk_algorithm = &issuer_certificate[12..13];
        let issuer_pk_length = &issuer_certificate[13..14];
        let issuer_pk_exponent_length = &issuer_certificate[14..15];
        let issuer_pk_leftmost_digits = &issuer_certificate[15..checksum_position];
        debug!("Issuer Identifier:{:02X?}", issuer_certificate_iin);
        debug!("Issuer expiry:{:02X?}", issuer_certificate_expiry);
        debug!("Issuer serial:{:02X?}", issuer_certificate_serial);
        debug!("Issuer hash algo:{:02X?}", issuer_certificate_hash_algorithm);
        debug!("Issuer pk algo:{:02X?}", issuer_pk_algorithm);
        debug!("Issuer pk length:{:02X?}", issuer_pk_length);
        debug!("Issuer pk exp length:{:02X?}", issuer_pk_exponent_length);
        debug!("Issuer pk leftmost digits:{:02X?}", issuer_pk_leftmost_digits);

        assert_eq!(issuer_certificate_hash_algorithm[0], 0x01); // SHA-1
        assert_eq!(issuer_pk_algorithm[0], 0x01); // RSA as defined in EMV Book 2, B2.1 RSA Algorihm

        let issuer_certificate_checksum = &issuer_certificate[checksum_position..checksum_position + 20];

        let mut checksum_data : Vec<u8> = Vec::new();
        checksum_data.extend_from_slice(&issuer_certificate[1..checksum_position]);
        checksum_data.extend_from_slice(&tag_92_issuer_pk_remainder[..]);
        checksum_data.extend_from_slice(&tag_9f32_issuer_pk_exponent[..]);

        let cert_checksum = sha::sha1(&checksum_data[..]);

        if &cert_checksum[..] != &issuer_certificate_checksum[..] {
            warn!("Issuer cert checksum mismatch!");
            warn!("Calculated checksum\n{}", HexViewBuilder::new(&cert_checksum[..]).finish());
            warn!("Issuer provided checksum\n{}", HexViewBuilder::new(&issuer_certificate_checksum[..]).finish());

            return Err(());
        }

        let tag_5a_pan = self.get_tag_value("5A").unwrap();
        let ascii_pan = bcdutil::bcd_to_ascii(&tag_5a_pan[..]).unwrap();
        let ascii_iin = bcdutil::bcd_to_ascii(&issuer_certificate_iin).unwrap();
        if ascii_iin != &ascii_pan[0..ascii_iin.len()] {
            warn!("IIN mismatch! Cert IIN: {:02X?}, PAN IIN: {:02X?}", ascii_iin, &ascii_pan[0..ascii_iin.len()]);

            return Err(());
        }

        is_certificate_expired(&issuer_certificate_expiry[..]);

        let mut issuer_pk_modulus : Vec<u8> = Vec::new();
        issuer_pk_modulus.extend_from_slice(issuer_pk_leftmost_digits);
        issuer_pk_modulus.extend_from_slice(&tag_92_issuer_pk_remainder[..]);
        trace!("Issuer PK modulus:\n{}", HexViewBuilder::new(&issuer_pk_modulus[..]).finish());

        Ok((issuer_pk_modulus, tag_9f32_issuer_pk_exponent.to_vec()))
    }


    pub fn get_icc_public_key(&self, icc_pk_certificate : &Vec<u8>, icc_pk_exponent : &Vec<u8>, icc_pk_remainder : Option<&Vec<u8>>, issuer_pk_modulus : &[u8], issuer_pk_exponent : &[u8], data_authentication : &[u8]) -> Result<(Vec<u8>, Vec<u8>), ()> {
        // ICC public key retrieval: EMV Book 2, 6.4 Retrieval of ICC Public Key
        debug!("Retrieving ICC public key {:02X?}", &icc_pk_certificate[0..2]);

        let tag_9f46_icc_pk_certificate = icc_pk_certificate;

        let issuer_pk = RsaPublicKey::new(issuer_pk_modulus, issuer_pk_exponent);
        let icc_certificate = issuer_pk.public_decrypt(&tag_9f46_icc_pk_certificate[..]).unwrap();
        let icc_certificate_length = icc_certificate.len();
        if icc_certificate[1] != 0x04 {
            warn!("Incorrect ICC certificate type {:02X?}", icc_certificate[1]);
            return Err(());
        }

        let checksum_position = 21 + icc_certificate_length-42;

        let icc_certificate_pan = &icc_certificate[2..12];
        let icc_certificate_expiry = &icc_certificate[12..14];
        let icc_certificate_serial = &icc_certificate[14..17];
        let icc_certificate_hash_algo = &icc_certificate[17..18];
        let icc_certificate_pk_algo = &icc_certificate[18..19];
        let icc_certificate_pk_length = &icc_certificate[19..20];
        let icc_certificate_pk_exp_length = &icc_certificate[20..21];
        let icc_certificate_pk_leftmost_digits = &icc_certificate[21..checksum_position];

        debug!("ICC PAN:{:02X?}", icc_certificate_pan);
        debug!("ICC expiry:{:02X?}", icc_certificate_expiry);
        debug!("ICC serial:{:02X?}", icc_certificate_serial);
        debug!("ICC hash algo:{:02X?}", icc_certificate_hash_algo);
        debug!("ICC pk algo:{:02X?}", icc_certificate_pk_algo);
        debug!("ICC pk length:{:02X?}", icc_certificate_pk_length);
        debug!("ICC pk exp length:{:02X?}", icc_certificate_pk_exp_length);
        debug!("ICC pk leftmost digits:{:02X?}", icc_certificate_pk_leftmost_digits);

        assert_eq!(icc_certificate_hash_algo[0], 0x01); // SHA-1
        assert_eq!(icc_certificate_pk_algo[0], 0x01); // RSA as defined in EMV Book 2, B2.1 RSA Algorihm

        let tag_9f47_icc_pk_exponent = icc_pk_exponent;

        let mut checksum_data : Vec<u8> = Vec::new();
        checksum_data.extend_from_slice(&icc_certificate[1..checksum_position]);

        let tag_9f48_icc_pk_remainder = icc_pk_remainder;
        if let Some(tag_9f48_icc_pk_remainder) = tag_9f48_icc_pk_remainder {
            checksum_data.extend_from_slice(&tag_9f48_icc_pk_remainder[..]);
        }

        checksum_data.extend_from_slice(&tag_9f47_icc_pk_exponent[..]);

        checksum_data.extend_from_slice(data_authentication);

        let static_data_authentication_tag_list_tag_values = self.get_tag_list_tag_values(&self.get_tag_value("9F4A").unwrap()[..]).unwrap();

        checksum_data.extend_from_slice(&static_data_authentication_tag_list_tag_values[..]);

        let cert_checksum = sha::sha1(&checksum_data[..]);

        let icc_certificate_checksum = &icc_certificate[checksum_position .. checksum_position + 20];

        trace!("Checksum data: {:02X?}", &checksum_data[..]);
        trace!("Calculated checksum: {:02X?}", cert_checksum);
        trace!("Stored ICC checksum: {:02X?}", icc_certificate_checksum);
        assert_eq!(cert_checksum, icc_certificate_checksum);

        let tag_5a_pan = self.get_tag_value("5A").unwrap();
        let ascii_pan = bcdutil::bcd_to_ascii(&tag_5a_pan[..]).unwrap();
        let icc_ascii_pan = bcdutil::bcd_to_ascii(&icc_certificate_pan).unwrap();
        if icc_ascii_pan != ascii_pan {
            warn!("PAN mismatch! Cert PAN: {:02X?}, PAN: {:02X?}", icc_ascii_pan, ascii_pan);

            return Err(());
        }

        is_certificate_expired(&icc_certificate_expiry[..]);

        let mut icc_pk_modulus : Vec<u8> = Vec::new();
        
        let icc_certificate_pk_leftmost_digits_length = icc_certificate_pk_leftmost_digits.iter()
            .rev().position(|c| -> bool { *c != 0xBB }).map(|i| icc_certificate_pk_leftmost_digits.len() - i).unwrap();

        icc_pk_modulus.extend_from_slice(&icc_certificate_pk_leftmost_digits[..icc_certificate_pk_leftmost_digits_length]);

        if let Some(tag_9f48_icc_pk_remainder) = tag_9f48_icc_pk_remainder {
            icc_pk_modulus.extend_from_slice(&tag_9f48_icc_pk_remainder[..]);
        }

        trace!("ICC PK modulus ({} bytes):\n{}", icc_pk_modulus.len(), HexViewBuilder::new(&icc_pk_modulus[..]).finish());

        Ok((icc_pk_modulus, tag_9f47_icc_pk_exponent.to_vec()))
    }

    pub fn handle_dynamic_data_authentication(&mut self, icc_pk_modulus : &[u8], icc_pk_exponent : &[u8]) -> Result<(),()> {
        debug!("Perform Dynamic Data Authentication (DDA):");

        let ddol_default_value = b"\x9f\x37\x04".to_vec();
        let tag_9f49_ddol = match self.get_tag_value("9F49") {
            Some(ddol) => ddol,
            // fall-back to a default DDOL
            None => &ddol_default_value
        };

        let ddol_data = self.get_tag_list_tag_values(&tag_9f49_ddol[..]).unwrap();

        let mut auth_data : Vec<u8> = Vec::new();
        auth_data.extend_from_slice(&ddol_data[..]);

        let apdu_command_internal_authenticate = b"\x00\x88\x00\x00";
        let mut internal_authenticate_command = apdu_command_internal_authenticate.to_vec();
        internal_authenticate_command.push(auth_data.len() as u8);
        internal_authenticate_command.extend_from_slice(&auth_data[..]);
        internal_authenticate_command.push(0x00);

        let (response_trailer, response_data) = self.send_apdu(&internal_authenticate_command);
        if !is_success_response(&response_trailer) {
            warn!("Could not process internal authenticate");
            return Err(());
        }

        if response_data[0] == 0x80 {
            self.add_tag("9F4B", response_data[3..].to_vec());
        } else if response_data[0] != 0x77 {
            warn!("Unrecognized response");
            return Err(());
        }

        let tag_9f4b_signed_data = self.get_tag_value("9F4B").unwrap();
        trace!("9F4B signed data result moduluslength:{}, ({} bytes):\n{}", icc_pk_modulus.len(), tag_9f4b_signed_data.len(), HexViewBuilder::new(&tag_9f4b_signed_data[..]).finish());

        let icc_pk = RsaPublicKey::new(icc_pk_modulus, icc_pk_exponent);
        let tag_9f4b_signed_data_decrypted = icc_pk.public_decrypt(&tag_9f4b_signed_data[..]).unwrap();
        let tag_9f4b_signed_data_decrypted_length = tag_9f4b_signed_data_decrypted.len();
        if tag_9f4b_signed_data_decrypted[1] != 0x05 {
            warn!("Unrecognized format");
            return Err(());
        }

        let tag_9f4b_signed_data_decrypted_hash_algo = tag_9f4b_signed_data_decrypted[2];
        assert_eq!(tag_9f4b_signed_data_decrypted_hash_algo, 0x01);

        let tag_9f4b_signed_data_decrypted_dynamic_data_length = tag_9f4b_signed_data_decrypted[3] as usize;
        
        let tag_9f4b_signed_data_decrypted_dynamic_data = &tag_9f4b_signed_data_decrypted[4..4+tag_9f4b_signed_data_decrypted_dynamic_data_length];
        let tag_9f4c_icc_dynamic_number = &tag_9f4b_signed_data_decrypted_dynamic_data[1..];
        self.add_tag("9F4C", tag_9f4c_icc_dynamic_number.to_vec());

        let checksum_position = tag_9f4b_signed_data_decrypted_length - 21;
        let mut checksum_data : Vec<u8> = Vec::new();
        checksum_data.extend_from_slice(&tag_9f4b_signed_data_decrypted[1..checksum_position]);
        checksum_data.extend_from_slice(&auth_data[..]);

        let signed_data_checksum = sha::sha1(&checksum_data[..]);

        let tag_9f4b_signed_data_decrypted_checksum = &tag_9f4b_signed_data_decrypted[checksum_position..checksum_position+20];

        if &signed_data_checksum[..] != &tag_9f4b_signed_data_decrypted_checksum[..] {
            warn!("Signed data checksum mismatch!");
            warn!("Calculated checksum\n{}", HexViewBuilder::new(&signed_data_checksum[..]).finish());
            warn!("Signed data checksum\n{}", HexViewBuilder::new(&tag_9f4b_signed_data_decrypted_checksum[..]).finish());

            return Err(());
        }

        Ok(())
    }

    // EMV has some tags that don't conform to ISO/IEC 7816
    fn is_non_conforming_one_byte_tag(&self, tag : u8) -> bool {
        if tag == 0x95 {
            return true;
        }

        false
    }

    fn get_tag_list_tag_values(&self, tag_list : &[u8]) -> Result<Vec<u8>, ()> {
        let mut output : Vec<u8> = Vec::new();

        if tag_list.len() < 2 {
            let tag_name = hex::encode(&tag_list[0..1]).to_uppercase();
            let value = match self.get_tag_value(&tag_name) {
                Some(value) => value,
                None => {
                    warn!("tag {:?} has no value", tag_name);
                    return Err(());
                }
            };

            output.extend_from_slice(&value[..]);
        } else {
            let mut i = 0;
            loop {
                let tag_value_length : usize;

                let mut tag_name = hex::encode(&tag_list[i..i+1]).to_uppercase();

                if Tag::try_from(tag_name.as_str()).is_ok() || self.is_non_conforming_one_byte_tag(tag_list[i]) {
                    tag_value_length = tag_list[i+1] as usize;
                    i += 2;
                } else {
                    tag_name = hex::encode(&tag_list[i..i+2]).to_uppercase();
                    if Tag::try_from(tag_name.as_str()).is_ok() {
                        tag_value_length = tag_list[i+2] as usize;
                        i += 3;
                    } else {
                        warn!("Incorrect tag {:?}", tag_name);
                        return Err(());
                    }
                }

                let value = match self.get_tag_value(&tag_name) {
                    Some(value) => value,
                    None => {
                        warn!("tag {:?} has no value", tag_name);
                        return Err(());
                    }
                };

                if value.len() != tag_value_length {
                    warn!("tag {:?} value length {:02X} does not match tag list value length {:02X}", tag_name, value.len(), tag_value_length);
                    return Err(());
                }

                output.extend_from_slice(&value[..]);

                if i >= tag_list.len() {
                    break;
                }
            }
        }

        Ok(output)
    }
}

pub struct EmvApplication {
    pub aid : Vec<u8>,
    pub label : Vec<u8>,
    pub priority : Vec<u8>
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EmvTag {
    pub tag: String,
    pub name: String
}
 
#[derive(Serialize, Deserialize)]
pub struct RsaPublicKey {
    pub modulus: String,
    pub exponent: String
}

impl RsaPublicKey {
    pub fn new(modulus : &[u8], exponent : &[u8]) -> RsaPublicKey {
        RsaPublicKey { modulus: hex::encode_upper(modulus), exponent: hex::encode_upper(exponent) }
    }

    pub fn public_encrypt(&self, plaintext_data : &[u8]) -> Result<Vec<u8>, ()> {
        let pk_modulus_raw = hex::decode(&self.modulus).unwrap();
        let pk_modulus = BigNum::from_slice(&pk_modulus_raw[..]).unwrap();
        let pk_exponent = BigNum::from_slice(&(hex::decode(&self.exponent).unwrap())[..]).unwrap();

        let rsa = Rsa::from_public_components(pk_modulus, pk_exponent).unwrap();

        let mut encrypt_output = [0u8; 4096];

        let length = match rsa.public_encrypt(plaintext_data, &mut encrypt_output[..], Padding::NONE) {
            Ok(length) => length,
            Err(_) => {
                warn!("Could not decrypt data");
                return Err(());
            }
        };

        let mut data = Vec::new();
        data.extend_from_slice(&encrypt_output[..length]);

        trace!("Encrypt result ({} bytes):\n{}", data.len(), HexViewBuilder::new(&data[..]).finish());

        if data.len() != pk_modulus_raw.len() {
            warn!("Data length discrepancy");
            return Err(());
        }

        Ok(data)
    }

    pub fn public_decrypt(&self, cipher_data : &[u8]) -> Result<Vec<u8>, ()> {
        let pk_modulus_raw = hex::decode(&self.modulus).unwrap();
        let pk_modulus = BigNum::from_slice(&pk_modulus_raw[..]).unwrap();
        let pk_exponent = BigNum::from_slice(&(hex::decode(&self.exponent).unwrap())[..]).unwrap();

        let rsa = Rsa::from_public_components(pk_modulus, pk_exponent).unwrap();

        let mut decrypt_output = [0u8; 4096];

        let length = match rsa.public_decrypt(cipher_data, &mut decrypt_output[..], Padding::NONE) {
            Ok(length) => length,
            Err(_) => {
                warn!("Could not decrypt data");
                return Err(());
            }
        };

        let mut data = Vec::new();
        data.extend_from_slice(&decrypt_output[..length]);

        trace!("Decrypt result ({} bytes):\n{}", data.len(), HexViewBuilder::new(&data[..]).finish());

        if data.len() != pk_modulus_raw.len() {
            warn!("Data length discrepancy");
            return Err(());
        }
        if data[0] != 0x6A {
            warn!("Data header incorrect");
            return Err(());
        }
        if data[data.len() - 1] != 0xBC {
            warn!("Data trailer incorrect");
            return Err(());
        }

        Ok(data)
    }
}

#[derive(Serialize, Deserialize)]
pub struct CertificateAuthority {
    issuer: String,
    certificates: HashMap<String, RsaPublicKey>
}

pub fn is_success_response(response_trailer : &Vec<u8>) -> bool {
    let mut success = false;

    if response_trailer.len() >= 2
        && response_trailer[0] == 0x90 && response_trailer[1] == 0x00 {
        success = true;
    }

    success
}

fn parse_tlv(raw_data : &[u8]) -> Option<Tlv> {
    let (tlv_data, leftover_buffer) = Tlv::parse(raw_data);
    if leftover_buffer.len() > 0 {
        trace!("Could not parse as TLV: {:02X?}", leftover_buffer);
    }

    let tlv_data : Option<Tlv> = match tlv_data {
        Ok(tlv) => Some(tlv),
        Err(_) => None

    };

    return tlv_data;
}

pub fn get_ca_public_key<'a>(ca_data : &'a HashMap<String, CertificateAuthority>, rid : &[u8], index : &[u8]) -> Option<&'a RsaPublicKey> {
    match ca_data.get(&hex::encode_upper(&rid)) {
        Some(ca) => {
            match ca.certificates.get(&hex::encode_upper(&index)) {
                Some(pk) => Some(pk),
                _ => None
            }
        },
        _ => None
    }
}

pub fn is_certificate_expired(date_bcd : &[u8]) -> bool {
    let today = Utc::today().naive_utc();
    let expiry_date = NaiveDate::parse_from_str(&format!("01{:02X?}", date_bcd), "%d[%m, %y]").unwrap();
    let duration = today.signed_duration_since(expiry_date).num_days();

    if duration > 30 {
        warn!("Certificate expiry date (MMYY) {:02X?} is {} days in the past", date_bcd, duration.to_string());

        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use hexplay::HexViewBuilder;
    use std::str;
    use std::sync::Once;
    use serde::{Deserialize, Serialize};
    use log4rs;
    use openssl::rsa::{Rsa, Padding};
    use hex;
    use std::fs::{self};
    use log::{debug, LevelFilter};
    use log4rs::{
        append::console::ConsoleAppender,
        config::{Appender, Root}
    };

    static LOGGING: Once = Once::new();

    #[derive(Serialize, Deserialize, Clone)]
    struct ApduRequestResponse {
        req : String,
        res : String
    }

    impl ApduRequestResponse {
        fn to_raw_vec(s : &String) -> Vec<u8> {
            hex::decode(s.replace(" ", "")).unwrap()
        }
    }

    fn find_dummy_apdu<'a>(test_data : &'a Vec<ApduRequestResponse>, apdu : &[u8]) -> Option<&'a ApduRequestResponse> {
        for data in test_data {
            if &apdu[..] == &ApduRequestResponse::to_raw_vec(&data.req)[..] {
                return Some(data);
            }
        }

        None
    }

    fn dummy_card_apdu_interface(apdu : &[u8]) -> Result<Vec<u8>, ()> {
        let mut output : Vec<u8> = Vec::new();

        let mut response = b"\x6A\x82".to_vec(); // file not found error

        let test_data : Vec<ApduRequestResponse> = serde_yaml::from_str(&fs::read_to_string("test_data.yaml").unwrap()).unwrap();

        if let Some(req) = find_dummy_apdu(&test_data, &apdu[..]) {
            response = ApduRequestResponse::to_raw_vec(&req.res);
        }

        output.extend_from_slice(&response[..]);
        Ok(output)
    }

    fn init_logging() {
        LOGGING.call_once(|| {
            let stdout: ConsoleAppender = ConsoleAppender::builder().build();
            let config = log4rs::config::Config::builder()
                .appender(Appender::builder().build("stdout", Box::new(stdout)))
                .build(Root::builder().appender("stdout").build(LevelFilter::Trace))
                .unwrap();
            log4rs::init_config(config).unwrap();
        });
    }

    #[test]
    fn test_rsa_key() -> Result<(), String> {
        init_logging();

        const KEY_SIZE : u32 = 1408;
        const KEY_BYTE_SIZE : usize = KEY_SIZE as usize / 8;

        // openssl key generation:
        // openssl genrsa -out icc_1234560012345608_e_3_private_key.pem -3 1024
        // openssl genrsa -out iin_313233343536_e_3_private_key.pem -3 1408
        // openssl genrsa -out AFFFFFFFFF_92_ca_private_key.pem -3 1408
        // openssl rsa -in AFFFFFFFFF_92_ca_private_key.pem -outform PEM -pubout -out AFFFFFFFFF_92_ca_key.pem

        let rsa = Rsa::private_key_from_pem(&fs::read_to_string("../config/AFFFFFFFFF_92_ca_private_key.pem").unwrap().as_bytes()).unwrap();
        //let rsa = Rsa::private_key_from_pem(&fs::read_to_string("../config/iin_313233343536_e_3_private_key.pem").unwrap().as_bytes()).unwrap();
        //let rsa = Rsa::private_key_from_pem(&fs::read_to_string("../config/icc_1234560012345608_e_3_private_key.pem").unwrap().as_bytes()).unwrap();
        
        let public_key_modulus  = &rsa.n().to_vec()[..];
        let public_key_exponent = &rsa.e().to_vec()[..];
        let private_key_exponent = &rsa.d().to_vec()[..];

        let pk = RsaPublicKey::new(public_key_modulus, public_key_exponent);
        debug!("modulus: {:02X?}, exponent: {:02X?}, private_exponent: {:02X?}", public_key_modulus, public_key_exponent, private_key_exponent);

        let mut encrypt_output = [0u8; KEY_BYTE_SIZE];
        let mut plaintext_data = [0u8; KEY_BYTE_SIZE];
        plaintext_data[0] = 0x6A;
        plaintext_data[1] = 0xFF;
        plaintext_data[KEY_BYTE_SIZE-1] = 0xBC;

        let encrypt_size = rsa.private_encrypt(&plaintext_data[..], &mut encrypt_output[..], Padding::NONE).unwrap();

        debug!("Encrypt result ({} bytes):\n{}", encrypt_output.len(), HexViewBuilder::new(&encrypt_output[..]).finish());

        let decrypted_data = pk.public_decrypt(&encrypt_output[0..encrypt_size]).unwrap();

        assert_eq!(&plaintext_data[..], &decrypted_data[..]);

        Ok(())
    }

    #[test]
    fn test_purchase_transaction() -> Result<(), String> {
        init_logging();

        let mut connection = EmvConnection::new().unwrap();
        connection.interface = Some(ApduInterface::Function(dummy_card_apdu_interface));

        let applications = connection.handle_select_payment_system_environment().unwrap();

        let application = &applications[0];
        connection.handle_select_payment_application(application).unwrap();

        connection.process_settings().unwrap();

        // force transaction date as 24.07.2020
        connection.add_tag("9A", b"\x20\x07\x24".to_vec());

        // force unpreditable number
        connection.add_tag("9F37", b"\x01\x23\x45\x67".to_vec());
        connection.settings.terminal.use_random = false;

        let search_tag = b"\x9f\x36";
        connection.handle_get_data(&search_tag[..]).unwrap();

        let data_authentication = connection.handle_get_processing_options().unwrap();

        let (issuer_pk_modulus, issuer_pk_exponent) = connection.get_issuer_public_key(application).unwrap();

        let tag_9f46_icc_pk_certificate = connection.get_tag_value("9F46").unwrap();
        let tag_9f47_icc_pk_exponent = connection.get_tag_value("9F47").unwrap();
        let tag_9f48_icc_pk_remainder = connection.get_tag_value("9F48");
        let (icc_pk_modulus, icc_pk_exponent) = connection.get_icc_public_key(
            tag_9f46_icc_pk_certificate, tag_9f47_icc_pk_exponent, tag_9f48_icc_pk_remainder,
            &issuer_pk_modulus[..], &issuer_pk_exponent[..],
            &data_authentication[..]).unwrap();

        connection.handle_dynamic_data_authentication(&icc_pk_modulus[..], &icc_pk_exponent[..]).unwrap();

        let ascii_pin = "1234".to_string();
        connection.handle_verify_plaintext_pin(ascii_pin.as_bytes()).unwrap();

        let icc_pin_pk_modulus = icc_pk_modulus.clone();
        let icc_pin_pk_exponent = icc_pk_exponent.clone();
        connection.handle_verify_enciphered_pin(ascii_pin.as_bytes(), &icc_pin_pk_modulus[..], &icc_pin_pk_exponent[..]).unwrap();

        // enciphered PIN OK
        connection.add_tag("9F34", b"\x44\x03\x02".to_vec());

        connection.handle_generate_ac().unwrap();

        Ok(())
    }
}