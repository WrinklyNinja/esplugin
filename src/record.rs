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
use std::convert::TryInto;
use std::io;
use std::num::NonZeroU32;

use nom::bytes::complete::take;
use nom::combinator::{cond, map};
use nom::number::complete::le_u32;
use nom::sequence::{delimited, pair, terminated, tuple};
use nom::IResult;

use crate::error::{Error, ParsingErrorKind};
use crate::game_id::GameId;
use crate::record_id::{NamespacedId, RecordId};
use crate::subrecord::{parse_subrecord_data_as_u32, Subrecord, SubrecordRef, SubrecordType};

const RECORD_TYPE_LENGTH: usize = 4;
pub type RecordType = [u8; 4];

#[derive(Clone, PartialEq, Eq, Debug, Hash, Default)]
pub struct RecordHeader {
    record_type: RecordType,
    flags: u32,
    form_id: Option<NonZeroU32>,
    size_of_subrecords: u32,
}

impl RecordHeader {
    fn are_subrecords_compressed(&self) -> bool {
        (self.flags & 0x0004_0000) != 0
    }

    pub fn flags(&self) -> u32 {
        self.flags
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Hash, Default)]
pub struct Record {
    header: RecordHeader,
    subrecords: Vec<Subrecord>,
}

impl Record {
    pub fn read_and_validate<T: io::Read>(
        reader: &mut T,
        game_id: GameId,
        expected_type: &[u8],
    ) -> Result<Vec<u8>, Error> {
        let mut content: Vec<u8> = vec![0; header_length(game_id)];
        reader.read_exact(&mut content)?;

        if &content[0..4] != expected_type {
            // Take a copy of 16 bytes so the output includes the FormID.
            return Err(Error::ParsingError(
                content[..16].to_vec(),
                ParsingErrorKind::UnexpectedRecordType(expected_type.to_vec()),
            ));
        }

        let size_of_subrecords = crate::le_slice_to_u32(&content[4..]) as usize;
        if size_of_subrecords > 0 {
            let mut subrecords = vec![0; size_of_subrecords];
            reader.read_exact(&mut subrecords)?;

            content.append(&mut subrecords);
        }

        Ok(content)
    }

    pub fn parse(input: &[u8], game_id: GameId, skip_subrecords: bool) -> IResult<&[u8], Record> {
        record(input, game_id, skip_subrecords)
    }

    pub fn read<R: std::io::Read>(
        reader: &mut R,
        game_id: GameId,
        skip_subrecords: bool,
    ) -> Result<Record, Error> {
        let mut header_bytes: Vec<u8> = vec![0; header_length(game_id)];
        reader.read_exact(&mut header_bytes)?;

        let header = all_consuming(record_header(&header_bytes, game_id))?;

        let mut subrecord_bytes: Vec<u8> = vec![0; header.size_of_subrecords as usize];
        reader.read_exact(&mut subrecord_bytes)?;

        let subrecords: Vec<Subrecord> = if !skip_subrecords {
            all_consuming(parse_subrecords(
                &subrecord_bytes,
                game_id,
                header.are_subrecords_compressed(),
            ))?
        } else {
            Vec::new()
        };

        Ok(Record { header, subrecords })
    }

    pub fn read_record_id<R: io::BufRead + io::Seek>(
        reader: &mut R,
        game_id: GameId,
        mut header_buffer: &mut [u8],
        header_already_read: bool,
    ) -> Result<(u32, Option<RecordId>), Error> {
        let header_length_read = if !header_already_read {
            let header_length = header_length(game_id);

            header_buffer = &mut header_buffer[..header_length];
            reader.read_exact(&mut header_buffer)?;
            header_length
        } else {
            0
        };

        let header = all_consuming(record_header(&header_buffer, game_id))?;

        if game_id == GameId::Morrowind {
            let mut subrecords_data = vec![0; header.size_of_subrecords as usize];
            reader.read_exact(&mut subrecords_data)?;

            let bytes_read = header_length_read as u32 + header.size_of_subrecords;

            let (_, record_id) = parse_morrowind_record_id_from_input(&subrecords_data, &header)?;
            Ok((bytes_read, record_id))
        } else {
            // Seeking discards the current buffer, so only do so if the data
            // to be skipped doesn't fit in the buffer anyway.
            let buffer = reader.fill_buf()?;
            if header.size_of_subrecords as usize > buffer.len() {
                reader.seek(io::SeekFrom::Current(i64::from(header.size_of_subrecords)))?;
            } else {
                reader.consume(header.size_of_subrecords as usize);
            }

            Ok((
                header_length_read as u32 + header.size_of_subrecords,
                header.form_id.map(RecordId::FormId),
            ))
        }
    }

    pub fn parse_record_id(input: &[u8], game_id: GameId) -> IResult<&[u8], Option<RecordId>> {
        if game_id == GameId::Morrowind {
            let (remaining_input, header) = record_header(input, game_id)?;
            let (remaining_input, subrecords_data) =
                take(header.size_of_subrecords)(remaining_input)?;

            let (_, record_id) = parse_morrowind_record_id_from_input(subrecords_data, &header)?;
            Ok((remaining_input, record_id))
        } else {
            let mut parser = tuple((
                delimited(take(RECORD_TYPE_LENGTH), le_u32, take(4usize)),
                terminated(le_u32, take(4usize)),
            ));

            let (remaining_input, (size_of_subrecords, form_id)) = parser(input)?;

            let mut parser = pair(
                cond(game_id != GameId::Oblivion, take(4usize)),
                take(size_of_subrecords),
            );

            let (remaining_input, _) = parser(remaining_input)?;

            Ok((
                remaining_input,
                NonZeroU32::new(form_id).map(RecordId::FormId),
            ))
        }
    }

    pub fn parse_record_id_from_self(&self, game_id: GameId) -> Option<RecordId> {
        if game_id == GameId::Morrowind {
            let types = record_id_subrecord_types(self.header.record_type);
            if !types.is_empty() {
                let subrecordrefs: Vec<SubrecordRef> = self
                    .subrecords
                    .iter()
                    .filter(|x| types.contains(&x.subrecord_type()))
                    .map(|x| SubrecordRef::from_subrecord(x))
                    .collect::<Vec<SubrecordRef>>();
                let data = record_id_subrecord_mapper(self.header.record_type, &subrecordrefs);

                data.map(|data| {
                    RecordId::NamespacedId(NamespacedId::new(self.header.record_type, data))
                })
            } else {
                None
            }
        } else {
            unimplemented!()
        }
    }

    pub fn header(&self) -> &RecordHeader {
        &self.header
    }

    pub fn header_type(&self) -> RecordType {
        self.header.record_type
    }

    pub fn subrecords(&self) -> &[Subrecord] {
        &self.subrecords
    }
}

fn parse_morrowind_record_id_from_input<'a>(
    subrecords_data: &'a [u8],
    header: &RecordHeader,
) -> IResult<&'a [u8], Option<RecordId>> {
    let types = record_id_subrecord_types(header.record_type);
    if !types.is_empty() {
        let (remaining_input, subrecords) = parse_id_subrecords(subrecords_data, types)?;
        let data = record_id_subrecord_mapper(header.record_type, &subrecords);

        let namespaced_id =
            data.map(|data| RecordId::NamespacedId(NamespacedId::new(header.record_type, data)));

        Ok((remaining_input, namespaced_id))
    } else {
        Ok((subrecords_data, None))
    }
}

fn all_consuming<I, T, E>(result: IResult<I, T, E>) -> Result<T, nom::Err<E>>
where
    I: nom::InputLength,
    E: nom::error::ParseError<I>,
{
    let (remaining_input, value) = result?;
    if remaining_input.input_len() == 0 {
        Ok(value)
    } else {
        Err(nom::Err::Error(E::from_error_kind(
            remaining_input,
            nom::error::ErrorKind::Eof,
        )))
    }
}

fn map_scpt_id_data<'a>(subrecords: &[SubrecordRef<'a>]) -> Option<&'a [u8]> {
    subrecords
        .first()
        .map(SubrecordRef::data)
        .filter(|d| d.len() > 32)
        .map(|d| &d[..32])
}

fn map_generic_id_data<'a>(subrecords: &[SubrecordRef<'a>]) -> Option<&'a [u8]> {
    subrecords.first().map(SubrecordRef::data)
}

fn map_cell_id_data<'a>(subrecords: &[SubrecordRef<'a>]) -> Option<&'a [u8]> {
    // Cell logic: if an exterior cell, use the data subrecord, otherwise
    // use the name. However, name appears before cell, so need to collect
    // both before deciding.
    if subrecords.len() != 2
        || subrecords[0].subrecord_type() != b"NAME"
        || subrecords[1].subrecord_type() != b"DATA"
    {
        return None;
    }

    let name = subrecords[0].data();
    let data = subrecords[1].data();

    if data.len() == 12 && data[0] & 0x01 == 0 {
        Some(&data[4..])
    } else if name.len() > 1 {
        Some(name)
    } else {
        None
    }
}

fn map_pgrd_id_data<'a>(subrecords: &[SubrecordRef<'a>]) -> Option<&'a [u8]> {
    // PGRD logic: use data subrecord if grid coord is not empty, otherwise
    // use name. data comes before name.
    if subrecords.len() != 2
        || subrecords[0].subrecord_type() != b"DATA"
        || subrecords[1].subrecord_type() != b"NAME"
    {
        return None;
    }

    let data = subrecords[0].data();
    let name = subrecords[1].data();

    if data.len() > 8 && data[..8] != [0; 8] {
        Some(&data[..8])
    } else {
        Some(name)
    }
}

fn record_id_subrecord_mapper<'a>(
    record_type: RecordType,
    subrecords: &[SubrecordRef<'a>],
) -> Option<&'a [u8]> {
    match &record_type {
        b"CELL" => map_cell_id_data(subrecords),
        b"PGRD" => map_pgrd_id_data(subrecords),
        b"SCPT" => map_scpt_id_data(subrecords),
        _ => map_generic_id_data(subrecords),
    }
}

fn record_id_subrecord_types(record_type: RecordType) -> Vec<&'static SubrecordType> {
    match &record_type {
        b"GMST" | b"GLOB" | b"CLAS" | b"FACT" | b"RACE" | b"SOUN" | b"REGN" | b"BSGN" | b"LTEX"
        | b"STAT" | b"DOOR" | b"MISC" | b"WEAP" | b"CONT" | b"SPEL" | b"CREA" | b"BODY"
        | b"LIGH" | b"ENCH" | b"NPC_" | b"ARMO" | b"CLOT" | b"REPA" | b"ACTI" | b"APPA"
        | b"LOCK" | b"PROB" | b"INGR" | b"BOOK" | b"ALCH" | b"LEVI" | b"LEVC" | b"SNDG"
        | b"DIAL" => vec![b"NAME"],
        b"SKIL" | b"MGEF" => vec![b"INDX"],
        // INFO handling is a little inaccurate, as identical INFO records can
        // appear after different DIAL records and still be different records.
        b"INFO" => vec![b"INAM"],
        b"LAND" => vec![b"INTV"],
        b"SCPT" => vec![b"SCHD"],
        b"CELL" | b"PGRD" => vec![b"DATA", b"NAME"],
        b"TES3" | _ => Vec::new(),
    }
}

fn header_length(game_id: GameId) -> usize {
    match game_id {
        GameId::Morrowind => 16,
        GameId::Oblivion => 20,
        _ => 24,
    }
}

fn record_type(input: &[u8]) -> IResult<&[u8], RecordType> {
    map(take(RECORD_TYPE_LENGTH), |s: &[u8]| {
        s.try_into()
            .expect("record type slice should be the required length")
    })(input)
}

fn record_header(input: &[u8], game_id: GameId) -> IResult<&[u8], RecordHeader> {
    map(
        tuple((
            record_type,
            le_u32,
            cond(game_id == GameId::Morrowind, take(4usize)),
            le_u32,
            cond(game_id != GameId::Morrowind, le_u32),
            cond(game_id != GameId::Morrowind, take(4usize)),
            cond(
                game_id != GameId::Morrowind && game_id != GameId::Oblivion,
                take(4usize),
            ),
        )),
        |(record_type, size_of_subrecords, _, flags, form_id, _, _)| RecordHeader {
            record_type,
            flags,
            form_id: form_id.and_then(NonZeroU32::new),
            size_of_subrecords,
        },
    )(input)
}

fn record(input: &[u8], game_id: GameId, skip_subrecords: bool) -> IResult<&[u8], Record> {
    let (remaining_input, header) = record_header(input, game_id)?;
    let (remaining_input, subrecords_data) = take(header.size_of_subrecords)(remaining_input)?;

    let subrecords: Vec<Subrecord> = if !skip_subrecords {
        parse_subrecords(subrecords_data, game_id, header.are_subrecords_compressed())?.1
    } else {
        Vec::new()
    };

    Ok((remaining_input, Record { header, subrecords }))
}

fn parse_subrecords(
    input: &[u8],
    game_id: GameId,
    are_compressed: bool,
) -> IResult<&[u8], Vec<Subrecord>> {
    let mut input1: &[u8] = input;
    let mut subrecords: Vec<Subrecord> = Vec::new();
    let mut large_subrecord_size: u32 = 0;

    while !input1.is_empty() {
        let (input2, subrecord) =
            Subrecord::new(input1, game_id, large_subrecord_size, are_compressed)?;

        if subrecord.subrecord_type() == b"XXXX" {
            large_subrecord_size = parse_subrecord_data_as_u32(input1)?.1;
        } else {
            large_subrecord_size = 0;
            subrecords.push(subrecord);
        }
        input1 = input2;
    }

    Ok((input1, subrecords))
}

/// Parses subrecords until one of each given subrecord type has been read, then
/// skips to the end of the record and returns.
fn parse_id_subrecords<'a>(
    input: &'a [u8],
    mut target_subrecord_types: Vec<&SubrecordType>,
) -> IResult<&'a [u8], Vec<SubrecordRef<'a>>> {
    let mut remaining_input: &[u8] = input;
    let mut subrecords = Vec::with_capacity(target_subrecord_types.len());

    while !remaining_input.is_empty() {
        if target_subrecord_types.is_empty() {
            break;
        }
        let (input2, subrecord) = SubrecordRef::new(remaining_input, GameId::Morrowind, 0)?;
        remaining_input = input2;
        if let Some(index) = target_subrecord_types
            .iter()
            .position(|m| m == &subrecord.subrecord_type())
        {
            subrecords.push(subrecord);
            target_subrecord_types.swap_remove(index);
        }
    }

    Ok((remaining_input, subrecords))
}

#[cfg(test)]
mod tests {
    use super::*;

    mod morrowind {
        use super::super::*;

        #[test]
        fn header_length_should_be_16() {
            assert_eq!(16, header_length(GameId::Morrowind));
        }

        #[test]
        fn parse_should_read_tes3_header_correctly() {
            let data =
                &include_bytes!("../testing-plugins/Morrowind/Data Files/Blank.esm")[..0x144];

            let record = Record::parse(data, GameId::Morrowind, false).unwrap().1;

            assert_eq!(0, record.header.flags);
            assert!(record.header.form_id.is_none());
            assert_eq!(1, record.subrecords.len());

            assert_eq!(b"HEDR", record.subrecords[0].subrecord_type());
        }

        #[test]
        fn parse_should_set_form_id_to_none_for_all_records() {
            let data =
                &include_bytes!("../testing-plugins/Morrowind/Data Files/Blank.esm")[0x144..0x16F];

            let record = Record::parse(data, GameId::Morrowind, false).unwrap().1;

            assert!(record.header.form_id.is_none());
        }

        #[test]
        fn parse_record_id_from_self_should_return_none_for_tes3_header() {
            let data =
                &include_bytes!("../testing-plugins/Morrowind/Data Files/Blank.esm")[..0x144];

            let record = Record::parse(data, GameId::Morrowind, false).unwrap().1;
            let form_id = record.parse_record_id_from_self(GameId::Morrowind);

            assert!(form_id.is_none());
        }

        #[test]
        fn parse_record_id_from_self_should_return_some_namespaced_id_for_gmst() {
            let data =
                &include_bytes!("../testing-plugins/Morrowind/Data Files/Blank.esm")[0x144..0x16F];

            let record = Record::parse(data, GameId::Morrowind, false).unwrap().1;
            let form_id = record.parse_record_id_from_self(GameId::Morrowind);

            assert!(form_id.unwrap().namespaced_id().is_some());
        }

        #[test]
        fn parse_record_id_should_return_none_for_tes3_header() {
            let data =
                &include_bytes!("../testing-plugins/Morrowind/Data Files/Blank.esm")[..0x144];

            let form_id = Record::parse_record_id(data, GameId::Morrowind).unwrap().1;

            assert!(form_id.is_none());
        }

        #[test]
        fn parse_record_id_should_return_some_namespaced_id_for_gmst() {
            let data =
                &include_bytes!("../testing-plugins/Morrowind/Data Files/Blank.esm")[0x144..0x16F];

            let form_id = Record::parse_record_id(data, GameId::Morrowind).unwrap().1;

            assert!(form_id.unwrap().namespaced_id().is_some());
        }

        #[test]
        fn record_id_subrecord_types_should_use_name_data_for_most_record_types() {
            let types = record_id_subrecord_types(*b"GMST");
            assert_eq!(vec![b"NAME"], types);
        }

        #[test]
        fn record_id_subrecord_types_should_use_indx_data_for_skil_records() {
            let types = record_id_subrecord_types(*b"SKIL");
            assert_eq!(vec![b"INDX"], types);
        }

        #[test]
        fn record_id_subrecord_types_should_use_indx_data_for_mgef_records() {
            let types = record_id_subrecord_types(*b"MGEF");
            assert_eq!(vec![b"INDX"], types);
        }

        #[test]
        fn record_id_subrecord_types_should_use_inam_data_for_info_records() {
            let types = record_id_subrecord_types(*b"INFO");
            assert_eq!(vec![b"INAM"], types);
        }

        #[test]
        fn record_id_subrecord_types_should_use_intv_data_for_land_records() {
            let types = record_id_subrecord_types(*b"LAND");
            assert_eq!(vec![b"INTV"], types);
        }

        #[test]
        fn record_id_subrecord_types_should_use_schd_data_for_scpt_records() {
            let types = record_id_subrecord_types(*b"SCPT");
            assert_eq!(vec![b"SCHD"], types);
        }

        #[test]
        fn record_id_subrecord_types_should_use_data_and_name_data_for_cell_records() {
            let types = record_id_subrecord_types(*b"CELL");
            assert_eq!(vec![b"DATA", b"NAME"], types);
        }

        #[test]
        fn record_id_subrecord_types_should_use_data_and_name_data_for_pgrd_records() {
            let types = record_id_subrecord_types(*b"PGRD");
            assert_eq!(vec![b"DATA", b"NAME"], types);
        }

        #[test]
        fn get_record_id_subrecord_mappers_should_return_an_empty_vec_for_the_tes3_record() {
            let types = record_id_subrecord_types(*b"TES3");
            assert!(types.is_empty());
        }

        #[test]
        fn get_record_id_subrecord_mappers_should_return_an_empty_vec_for_unrecognised_types() {
            let types = record_id_subrecord_types(*b"DUMY");
            assert!(types.is_empty());
        }

        #[test]
        fn record_id_subrecord_mapper_should_get_first_subrecords_data_for_most_record_types() {
            let input = &[
                0x4E, 0x41, 0x4D, 0x45, 0x11, 0x00, 0x00, 0x00, 0x73, 0x4D, 0x6F, 0x6E, 0x74, 0x68,
                0x4D, 0x6F, 0x72, 0x6E, 0x69, 0x6E, 0x67, 0x73, 0x74, 0x61, 0x72,
            ];
            let subrecords = &[SubrecordRef::new(input, GameId::Morrowind, 0).unwrap().1];

            let data = record_id_subrecord_mapper(*b"GMST", subrecords);
            assert_eq!(&input[8..], data.unwrap());
        }

        #[test]
        fn record_id_subrecord_mapper_should_get_first_32_bytes_of_first_subrecords_data_for_scpt_records(
        ) {
            let input = &[
                0x53, 0x43, 0x48, 0x44, 0x21, 0x00, 0x00, 0x00, 0x41, 0x62, 0x65, 0x62, 0x61, 0x61,
                0x6C, 0x41, 0x74, 0x74, 0x61, 0x63, 0x6B, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ];
            let subrecords = &[SubrecordRef::new(input, GameId::Morrowind, 0).unwrap().1];
            let data = record_id_subrecord_mapper(*b"SCPT", subrecords);

            assert_eq!(&input[8..40], data.unwrap());

            let input = &[
                0x53, 0x43, 0x48, 0x44, 0x0E, 0x00, 0x00, 0x00, 0x41, 0x62, 0x65, 0x62, 0x61, 0x61,
                0x6C, 0x41, 0x74, 0x74, 0x61, 0x63, 0x6B, 0x00,
            ];
            let subrecords = &[SubrecordRef::new(input, GameId::Morrowind, 0).unwrap().1];
            let data = record_id_subrecord_mapper(*b"SCPT", subrecords);

            assert!(data.is_none());
        }

        #[test]
        fn record_id_subrecord_mapper_should_use_data_for_exterior_cell_records() {
            let input = &[
                0x4E, 0x41, 0x4D, 0x45, 0x09, 0x00, 0x00, 0x00, 0x42, 0x61, 0x6C, 0x20, 0x46, 0x65,
                0x6C, 0x6C, 0x00, 0x44, 0x41, 0x54, 0x41, 0x0C, 0x00, 0x00, 0x00, 0x46, 0x00, 0x00,
                0x00, 0x08, 0x00, 0x00, 0x00, 0xF4, 0xFF, 0xFF, 0xFF,
            ];
            let subrecords = &[
                SubrecordRef::new(&input[..17], GameId::Morrowind, 0)
                    .unwrap()
                    .1,
                SubrecordRef::new(&input[17..], GameId::Morrowind, 0)
                    .unwrap()
                    .1,
            ];
            let data = record_id_subrecord_mapper(*b"CELL", subrecords);

            assert_eq!(&input[29..], data.unwrap());
        }

        #[test]
        fn record_id_subrecord_mapper_should_use_name_for_interior_cell_records() {
            let input = &[
                0x4E, 0x41, 0x4D, 0x45, 0x09, 0x00, 0x00, 0x00, 0x42, 0x61, 0x6C, 0x20, 0x46, 0x65,
                0x6C, 0x6C, 0x00, 0x44, 0x41, 0x54, 0x41, 0x0C, 0x00, 0x00, 0x00, 0x47, 0x00, 0x00,
                0x00, 0x08, 0x00, 0x00, 0x00, 0xF4, 0xFF, 0xFF, 0xFF,
            ];
            let subrecords = &[
                SubrecordRef::new(&input[..17], GameId::Morrowind, 0)
                    .unwrap()
                    .1,
                SubrecordRef::new(&input[17..], GameId::Morrowind, 0)
                    .unwrap()
                    .1,
            ];
            let data = record_id_subrecord_mapper(*b"CELL", subrecords);

            assert_eq!(&input[8..17], data.unwrap());
        }

        #[test]
        fn record_id_subrecord_mapper_should_return_none_for_interior_cell_records_with_no_name() {
            let input = &[
                0x4E, 0x41, 0x4D, 0x45, 0x01, 0x00, 0x00, 0x00, 0x00, 0x44, 0x41, 0x54, 0x41, 0x0C,
                0x00, 0x00, 0x00, 0x47, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0xF4, 0xFF, 0xFF,
                0xFF,
            ];
            let subrecords = &[
                SubrecordRef::new(&input[..17], GameId::Morrowind, 0)
                    .unwrap()
                    .1,
                SubrecordRef::new(&input[9..], GameId::Morrowind, 0)
                    .unwrap()
                    .1,
            ];
            let data = record_id_subrecord_mapper(*b"CELL", subrecords);

            assert!(data.is_none());
        }

        #[test]
        fn record_id_subrecord_mapper_should_use_data_for_exterior_pgrd_records() {
            let input = &[
                0x44, 0x41, 0x54, 0x41, 0x0C, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0xF4, 0xFF,
                0xFF, 0xFF, 0x00, 0x04, 0x37, 0x00, 0x4E, 0x41, 0x4D, 0x45, 0x09, 0x00, 0x00, 0x00,
                0x42, 0x61, 0x6C, 0x20, 0x46, 0x65, 0x6C, 0x6C, 0x00,
            ];
            let subrecords = &[
                SubrecordRef::new(&input[..20], GameId::Morrowind, 0)
                    .unwrap()
                    .1,
                SubrecordRef::new(&input[20..], GameId::Morrowind, 0)
                    .unwrap()
                    .1,
            ];
            let data = record_id_subrecord_mapper(*b"PGRD", subrecords);

            assert_eq!(&input[8..16], data.unwrap());
        }

        #[test]
        fn record_id_subrecord_mapper_should_use_name_for_interior_pgrd_records() {
            let input = &[
                0x44, 0x41, 0x54, 0x41, 0x0C, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x04, 0x37, 0x00, 0x4E, 0x41, 0x4D, 0x45, 0x09, 0x00, 0x00, 0x00,
                0x42, 0x61, 0x6C, 0x20, 0x46, 0x65, 0x6C, 0x6C, 0x00,
            ];
            let subrecords = &[
                SubrecordRef::new(&input[..20], GameId::Morrowind, 0)
                    .unwrap()
                    .1,
                SubrecordRef::new(&input[20..], GameId::Morrowind, 0)
                    .unwrap()
                    .1,
            ];
            let data = record_id_subrecord_mapper(*b"PGRD", subrecords);

            assert_eq!(&input[28..], data.unwrap());
        }
    }

    mod oblivion {
        use super::super::*;

        #[test]
        fn header_length_should_be_20() {
            assert_eq!(20, header_length(GameId::Oblivion));
        }

        #[test]
        fn parse_should_read_tes4_header_correctly() {
            let data = &include_bytes!("../testing-plugins/Oblivion/Data/Blank.esm")[..0x144];

            let record = Record::parse(data, GameId::Oblivion, false).unwrap().1;

            assert_eq!(1, record.header.flags);
            assert!(record.header.form_id.is_none());
            assert_eq!(3, record.subrecords.len());

            assert_eq!(b"HEDR", record.subrecords[0].subrecord_type());
            assert_eq!(b"CNAM", record.subrecords[1].subrecord_type());
            assert_eq!(b"SNAM", record.subrecords[2].subrecord_type());
        }

        #[test]
        fn parse_should_set_non_zero_form_id_for_non_header_records() {
            let data = &include_bytes!("../testing-plugins/Oblivion/Data/Blank.esm")[0x4C..0x70];

            let record = Record::parse(data, GameId::Oblivion, false).unwrap().1;

            assert_eq!(0xCF0, record.header.form_id.unwrap().get());
        }

        #[test]
        fn parse_record_id_should_return_the_form_id() {
            let data = &include_bytes!("../testing-plugins/Oblivion/Data/Blank.esm")[0x4C..0x70];

            let form_id = Record::parse_record_id(data, GameId::Oblivion).unwrap().1;

            assert_eq!(0xCF0, form_id.unwrap().form_id().unwrap().get());
        }
    }

    mod skyrim {
        use super::super::*;

        #[test]
        fn header_length_should_be_24() {
            assert_eq!(24, header_length(GameId::Skyrim));
        }

        #[test]
        fn parse_should_read_tes4_header_correctly() {
            let data =
                &include_bytes!("../testing-plugins/Skyrim/Data/Blank - Master Dependent.esm")
                    [..0x56];

            let record = Record::parse(data, GameId::Skyrim, false).unwrap().1;

            assert_eq!(0x1, record.header.flags);
            assert!(record.header.form_id.is_none());
            assert_eq!(5, record.subrecords.len());

            assert_eq!(b"HEDR", record.subrecords[0].subrecord_type());
            assert_eq!(b"CNAM", record.subrecords[1].subrecord_type());
            assert_eq!(b"SNAM", record.subrecords[2].subrecord_type());
            assert_eq!(b"MAST", record.subrecords[3].subrecord_type());
            assert_eq!(b"DATA", record.subrecords[4].subrecord_type());
        }

        #[test]
        fn parse_should_set_non_zero_form_id_for_non_header_records() {
            let data = &include_bytes!("../testing-plugins/Skyrim/Data/Blank.esp")[0x53..0xEF];

            let record = Record::parse(data, GameId::Skyrim, false).unwrap().1;

            assert_eq!(0xCEC, record.header.form_id.unwrap().get());
        }

        #[test]
        fn parse_record_id_should_return_the_form_id() {
            let data = &include_bytes!("../testing-plugins/Skyrim/Data/Blank.esp")[0x53..0xEF];

            let form_id = Record::parse_record_id(data, GameId::Skyrim).unwrap().1;

            assert_eq!(0xCEC, form_id.unwrap().form_id().unwrap().get());
        }
    }

    #[test]
    fn read_and_validate_should_read_a_record_from_the_given_reader() {
        let data =
            &include_bytes!("../testing-plugins/Skyrim/Data/Blank - Master Dependent.esm")[..0x56];
        let mut reader = io::Cursor::new(data);

        let bytes = Record::read_and_validate(&mut reader, GameId::Skyrim, b"TES4").unwrap();

        assert_eq!(data, bytes.as_slice());
    }

    #[test]
    fn read_and_validate_should_fail_if_the_type_is_unexpected() {
        let data =
            &include_bytes!("../testing-plugins/Skyrim/Data/Blank - Master Dependent.esm")[..0x56];
        let mut reader = io::Cursor::new(data);

        let result = Record::read_and_validate(&mut reader, GameId::Skyrim, b"TES3");
        assert!(result.is_err());
        assert_eq!("An error was encountered while parsing the plugin content [54, 45, 53, 34, 3E, 00, 00, 00, 01, 00, 00, 00, 00, 00, 00, 00]: Expected record type [54, 45, 53, 33]", result.unwrap_err().to_string());
    }

    #[test]
    fn parse_should_obey_skip_subrecords_parameter() {
        let data = &include_bytes!("../testing-plugins/Skyrim/Data/Blank.esm")[..0x1004C];

        let record = Record::parse(data, GameId::Skyrim, true).unwrap().1;

        assert_eq!(1, record.header.flags);
        assert_eq!(0, record.subrecords.len());
    }

    #[test]
    fn parse_should_read_large_subrecords_correctly() {
        let data = &include_bytes!("../testing-plugins/Skyrim/Data/Blank.esm")[..0x1004C];

        let record = Record::parse(data, GameId::Skyrim, false).unwrap().1;

        assert_eq!(0x1, record.header.flags);
        assert_eq!(4, record.subrecords.len());

        assert_eq!(b"HEDR", record.subrecords[0].subrecord_type());
        assert_eq!(b"CNAM", record.subrecords[1].subrecord_type());
        assert_eq!(b"SNAM", record.subrecords[2].subrecord_type());
        assert_eq!(b"ONAM", record.subrecords[3].subrecord_type());
    }

    #[test]
    #[cfg(feature = "compressed-fields")]
    fn parse_should_read_compressed_subrecords_correctly() {
        const DATA: &'static [u8] = &[
            0x42, 0x50, 0x54, 0x44, //type
            0x23, 0x00, 0x00, 0x00, //size
            0x00, 0x00, 0x04, 0x00, //flags
            0xEC, 0x0C, 0x00, 0x00, //id
            0x00, 0x00, 0x00, 0x00, //revision
            0x2B, 0x00, //version
            0x00, 0x00, //unknown
            0x42, 0x50, 0x54, 0x4E, //field type
            0x1D, 0x00, //field size
            0x19, 0x00, 0x00, 0x00, //decompressed field size
            0x75, 0xc5, 0x21, 0x0d, 0x00, 0x00, 0x08, 0x05, 0xd1,
            0x6c, //field data (compressed)
            0x6c, 0xdc, 0x57, 0x48, 0x3c, 0xfd, 0x5b, 0x5c, 0x02,
            0xd4, //field data (compressed)
            0x6b, 0x32, 0xb5, 0xdc, 0xa3, //field data (compressed)
        ];

        let record = Record::parse(DATA, GameId::Skyrim, false).unwrap().1;

        assert_eq!(0xCEC, record.header.form_id.unwrap().get());
        assert_eq!(0x00040000, record.header.flags);
        assert_eq!(1, record.subrecords.len());

        let decompressed_data = record.subrecords[0].decompress_data().unwrap();
        assert_eq!(
            "DEFLATE_DEFLATE_DEFLATE_DEFLATE".as_bytes(),
            decompressed_data.as_slice()
        );
    }
}
