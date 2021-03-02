/*
 * This file is part of esplugin
 *
 * Copyright (C) 2017 Oliver Hamlet
 *
 * esplugin is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * esplugin is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with esplugin. If not, see <http://www.gnu.org/licenses/>.
 */

extern crate nom;

use std::convert::TryInto;
#[cfg(feature = "compressed-fields")]
use std::io;
#[cfg(feature = "compressed-fields")]
use std::io::Read;

#[cfg(feature = "compressed-fields")]
use flate2::read::DeflateDecoder;

use nom::bytes::complete::take;
use nom::combinator::map;
use nom::multi::length_data;
use nom::number::complete::{le_u16, le_u32};
use nom::sequence::{pair, preceded, separated_pair, tuple};
use nom::IResult;

use crate::game_id::GameId;

const SUBRECORD_TYPE_LENGTH: usize = 4;
pub type SubrecordType = [u8; 4];

#[derive(Clone, PartialEq, Eq, Debug, Hash, Default)]
pub struct Subrecord {
    subrecord_type: SubrecordType,
    data: Vec<u8>,
    is_compressed: bool,
}

impl Subrecord {
    pub fn new(
        input: &[u8],
        game_id: GameId,
        data_length_override: u32,
        is_compressed: bool,
    ) -> IResult<&[u8], Subrecord> {
        let (remaining_input, (subrecord_type, data)) =
            parse(input, game_id, data_length_override)?;
        Ok((
            remaining_input,
            Subrecord {
                subrecord_type,
                data: data.to_vec(),
                is_compressed,
            },
        ))
    }

    #[cfg(feature = "compressed-fields")]
    pub fn decompress_data(&self) -> Result<Vec<u8>, io::Error> {
        if !self.is_compressed {
            return Ok(self.data.clone());
        }

        if self.data.len() < 5 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Compressed subrecord is too small",
            ));
        }

        let mut deflater = DeflateDecoder::new(&self.data[4..]);
        let mut decompressed_data: Vec<u8> = Vec::new();
        deflater.read_to_end(&mut decompressed_data)?;

        Ok(decompressed_data)
    }

    pub fn subrecord_type(&self) -> &SubrecordType {
        &self.subrecord_type
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

pub struct SubrecordRef<'a> {
    subrecord_type: SubrecordType,
    data: &'a [u8],
}

impl<'a> SubrecordRef<'a> {
    pub fn new(
        input: &'a [u8],
        game_id: GameId,
        data_length_override: u32,
    ) -> IResult<&'a [u8], SubrecordRef<'a>> {
        let (remaining_input, (subrecord_type, data)) =
            parse(input, game_id, data_length_override)?;

        Ok((
            remaining_input,
            SubrecordRef {
                subrecord_type,
                data,
            },
        ))
    }

    pub fn from_subrecord(subrecord: &'a Subrecord) -> SubrecordRef<'a> {
        SubrecordRef {
            subrecord_type: *subrecord.subrecord_type(),
            data: &subrecord.data,
        }
    }

    pub fn subrecord_type(&'a self) -> &SubrecordType {
        &self.subrecord_type
    }

    pub fn data(&self) -> &'a [u8] {
        self.data
    }
}

fn parse(
    input: &[u8],
    game_id: GameId,
    data_length_override: u32,
) -> IResult<&[u8], (SubrecordType, &[u8])> {
    if game_id == GameId::Morrowind {
        morrowind_subrecord(input)
    } else if data_length_override != 0 {
        presized_subrecord(input, data_length_override)
    } else {
        simple_subrecord(input)
    }
}

fn subrecord_type(input: &[u8]) -> IResult<&[u8], SubrecordType> {
    map(take(SUBRECORD_TYPE_LENGTH), |s: &[u8]| {
        s.try_into()
            .expect("subrecord type slice should be the required length")
    })(input)
}

fn morrowind_subrecord(input: &[u8]) -> IResult<&[u8], (SubrecordType, &[u8])> {
    tuple((subrecord_type, length_data(le_u32)))(input)
}

fn simple_subrecord(input: &[u8]) -> IResult<&[u8], (SubrecordType, &[u8])> {
    tuple((subrecord_type, length_data(le_u16)))(input)
}

fn presized_subrecord(input: &[u8], data_length: u32) -> IResult<&[u8], (SubrecordType, &[u8])> {
    separated_pair(subrecord_type, le_u16, take(data_length))(input)
}

pub fn parse_subrecord_data_as_u32(input: &[u8]) -> IResult<&[u8], u32> {
    preceded(pair(subrecord_type, le_u16), le_u32)(input)
}

#[cfg(test)]
mod tests {
    mod morrowind {
        use super::super::*;

        const TES3_DATA_SUBRECORD: &'static [u8] = &[
            0x44, 0x41, 0x54, 0x41, 0x08, 0x00, 0x00, 0x00, 0x6D, 0x63, 0x61, 0x72, 0x6F, 0x66,
            0x61, 0x6E,
        ];

        #[test]
        fn parse_should_parse_a_subrecord_correctly() {
            let subrecord = Subrecord::new(TES3_DATA_SUBRECORD, GameId::Morrowind, 0, false)
                .unwrap()
                .1;

            assert_eq!(b"DATA", &subrecord.subrecord_type);
            assert_eq!(&TES3_DATA_SUBRECORD[8..], subrecord.data.as_slice());
        }

        #[test]
        fn parse_should_ignore_data_length_override_subrecords() {
            let subrecord = Subrecord::new(TES3_DATA_SUBRECORD, GameId::Morrowind, 5, false)
                .unwrap()
                .1;

            assert_eq!(b"DATA", &subrecord.subrecord_type);
            assert_eq!(&TES3_DATA_SUBRECORD[8..], subrecord.data.as_slice());
        }

        #[test]
        fn subrecord_ref_new_should_parse_a_subrecord_correctly() {
            let subrecord = SubrecordRef::new(TES3_DATA_SUBRECORD, GameId::Morrowind, 0)
                .unwrap()
                .1;

            assert_eq!(b"DATA", subrecord.subrecord_type());
            assert_eq!(&TES3_DATA_SUBRECORD[8..], subrecord.data());
        }
    }

    mod nonmorrowind {
        use super::super::*;

        const TES4_CNAM_SUBRECORD: &'static [u8] = &[
            0x43, 0x4E, 0x41, 0x4D, 0x0A, 0x00, 0x6D, 0x63, 0x61, 0x72, 0x6F, 0x66, 0x61, 0x6E,
            0x6F, 0x00,
        ];

        #[test]
        fn parse_should_parse_a_subrecord_with_no_data_length_override_correctly() {
            let subrecord = Subrecord::new(TES4_CNAM_SUBRECORD, GameId::Skyrim, 0, false)
                .unwrap()
                .1;

            assert_eq!(b"CNAM", &subrecord.subrecord_type);
            assert_eq!(&TES4_CNAM_SUBRECORD[6..], subrecord.data.as_slice());
        }

        #[test]
        fn parse_should_use_data_length_override_if_non_zero() {
            let subrecord = Subrecord::new(TES4_CNAM_SUBRECORD, GameId::Oblivion, 4, false)
                .unwrap()
                .1;

            assert_eq!(b"CNAM", &subrecord.subrecord_type);
            assert_eq!(&TES4_CNAM_SUBRECORD[6..10], subrecord.data.as_slice());

            let subrecord = Subrecord::new(TES4_CNAM_SUBRECORD, GameId::Skyrim, 4, false)
                .unwrap()
                .1;

            assert_eq!(b"CNAM", &subrecord.subrecord_type);
            assert_eq!(&TES4_CNAM_SUBRECORD[6..10], subrecord.data.as_slice());

            let subrecord = Subrecord::new(TES4_CNAM_SUBRECORD, GameId::Fallout3, 4, false)
                .unwrap()
                .1;

            assert_eq!(b"CNAM", &subrecord.subrecord_type);
            assert_eq!(&TES4_CNAM_SUBRECORD[6..10], subrecord.data.as_slice());

            let subrecord = Subrecord::new(TES4_CNAM_SUBRECORD, GameId::FalloutNV, 4, false)
                .unwrap()
                .1;

            assert_eq!(b"CNAM", &subrecord.subrecord_type);
            assert_eq!(&TES4_CNAM_SUBRECORD[6..10], subrecord.data.as_slice());

            let subrecord = Subrecord::new(TES4_CNAM_SUBRECORD, GameId::Fallout4, 4, false)
                .unwrap()
                .1;

            assert_eq!(b"CNAM", &subrecord.subrecord_type);
            assert_eq!(&TES4_CNAM_SUBRECORD[6..10], subrecord.data.as_slice());
        }

        #[test]
        fn subrecord_ref_new_should_parse_a_subrecord_correctly() {
            let subrecord = SubrecordRef::new(TES4_CNAM_SUBRECORD, GameId::Skyrim, 0)
                .unwrap()
                .1;

            assert_eq!(b"CNAM", subrecord.subrecord_type());
            assert_eq!(&TES4_CNAM_SUBRECORD[6..], subrecord.data());
        }

        #[test]
        #[cfg(feature = "compressed-fields")]
        fn decompress_data_should_read_a_compressed_subrecord_correctly() {
            const DATA: &'static [u8] = &[
                0x42, 0x50, 0x54, 0x4E, //field type
                0x1D, 0x00, //field size
                0x19, 0x00, 0x00, 0x00, //decompressed field size
                0x75, 0xc5, 0x21, 0x0d, 0x00, 0x00, 0x08, 0x05, 0xd1,
                0x6c, //field data (compressed)
                0x6c, 0xdc, 0x57, 0x48, 0x3c, 0xfd, 0x5b, 0x5c, 0x02,
                0xd4, //field data (compressed)
                0x6b, 0x32, 0xb5, 0xdc, 0xa3, //field data (compressed)
            ];

            let subrecord = Subrecord::new(DATA, GameId::Skyrim, 0, true).unwrap().1;

            let decompressed_data = subrecord.decompress_data().unwrap();

            assert_eq!(b"BPTN", &subrecord.subrecord_type);
            assert_eq!(
                "DEFLATE_DEFLATE_DEFLATE_DEFLATE".as_bytes(),
                decompressed_data.as_slice()
            );
        }

        #[test]
        #[cfg(feature = "compressed-fields")]
        fn decompress_data_should_error_if_the_compressed_data_is_invalid() {
            const DATA: &'static [u8] = &[
                0x42, 0x50, 0x54, 0x4E, //field type
                0x1D, 0x00, //field size
                0x19, 0x00, 0x00, 0x00, //decompressed field size
                0x75, 0xc5, 0x21, 0x0d, 0x00, 0x00, 0xA8, 0x05, 0xd1,
                0x6c, //field data (compressed)
                0x6c, 0xdc, 0x57, 0x48, 0x3c, 0xfd, 0x5b, 0x5c, 0x02,
                0xd4, //field data (compressed)
                0x6b, 0x32, 0xb5, 0xdc, 0xa3, //field data (compressed)
            ];

            let subrecord = Subrecord::new(DATA, GameId::Skyrim, 0, true).unwrap().1;

            assert!(subrecord.decompress_data().is_err());
        }

        #[test]
        #[cfg(feature = "compressed-fields")]
        fn decompress_data_should_error_if_the_compressed_data_is_too_small() {
            const DATA: &'static [u8] = &[
                0x42, 0x50, 0x54, 0x4E, //field type
                0x02, 0x00, //field size
                0x19, 0x00, //field data
            ];

            let subrecord = Subrecord::new(DATA, GameId::Skyrim, 0, true).unwrap().1;

            assert!(subrecord.decompress_data().is_err());
        }
    }
}
