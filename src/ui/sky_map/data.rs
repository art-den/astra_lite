#![allow(dead_code)]

use std::{collections::*, f64::consts::PI, fmt::Debug, io::{BufRead, Read}, path::Path};
use bitflags::bitflags;
use bitstream_io::{BigEndian, BitReader};
use serde::{Deserialize, Serialize};
use crate::{indi::sexagesimal_to_value, utils::compression::ValuesDecompressor, sky_math::math::*};

enum SearchMode {
    StartWith,
    Contains,
}

#[derive(Debug, Clone, Default)]
pub struct Observer {
    pub latitude:  f64, // ϕ₀
    pub longitude: f64, // λ₀
}

#[derive(Copy, Clone)]
pub struct ObjEqCoord {
    ra:  u32,
    dec: i32,
}

impl ObjEqCoord {
    pub const OBJ_COORD_RA_DIV: f64 = 100_000_000_f64;
    pub const OBJ_COORD_DEC_DIV: f64 = 100_000_000_f64;

    pub fn new(ra: f64, dec: f64) -> Self {
        Self {
            ra: (ra * Self::OBJ_COORD_RA_DIV).round() as u32,
            dec: (dec * Self::OBJ_COORD_DEC_DIV).round() as i32,
        }
    }

    pub fn new_from_int(ra: u32, dec: i32) -> Self {
        Self { ra, dec }
    }

    pub fn to_eq(&self) -> EqCoord {
        EqCoord {
            ra: self.ra(),
            dec: self.dec(),
        }
    }

    pub fn ra(&self) -> f64 {
        self.ra as f64 / Self::OBJ_COORD_RA_DIV
    }

    pub fn dec(&self) -> f64 {
        self.dec as f64 / Self::OBJ_COORD_DEC_DIV
    }

    pub fn ra_int(&self) -> u32 {
        self.ra
    }

    pub fn dec_int(&self) -> i32 {
        self.dec
    }
}

impl Debug for ObjEqCoord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjCoord")
            .field("ra", &self.ra())
            .field("dec", &self.dec())
            .finish()
    }
}

#[test]
fn test_obj_coord() {
    let ra = hour_to_radian(24.0);
    let dec = degree_to_radian(90.0);
    let obj = ObjEqCoord::new(ra, dec);
    assert!(f64::abs(obj.ra()-ra) < 1.0 / ObjEqCoord::OBJ_COORD_RA_DIV);
    assert!(f64::abs(obj.dec()-dec) < 1.0 / ObjEqCoord::OBJ_COORD_RA_DIV);
}

/// Brightness
#[derive(Copy, Clone, PartialEq, PartialOrd)]
pub struct ObjMagnitude(i16);

impl ObjMagnitude {
    const OBJ_MAG_DIV: f32 = 100.0;

    pub fn new(value: f32) -> Self {
        let int_value = if value.is_nan() {
            i16::MIN
        } else {
            let mut result = (value * Self::OBJ_MAG_DIV).round() as i16;
            if result == i16::MIN {
                result = i16::MIN + 1;
            }
            result
        };
        Self(int_value)
    }

    pub fn get(&self) -> f32 {
        if self.0 == i16::MIN {
            f32::NAN
        } else {
            self.0 as f32 / Self::OBJ_MAG_DIV
        }
    }

    pub fn is_greater_than(self, other: Self) -> bool {
        self.0 > other.0
    }
}

impl Debug for ObjMagnitude {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ObjMagnitude").field(&self.get()).finish()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct StarBV(i16);

impl StarBV {
    pub fn new(bv: f32) -> Self {
        Self((bv * 1000.0) as i16)
    }

    pub fn get(&self) -> f32 {
        (self.0 as f32) / 1000.0
    }
}

#[derive(Clone, Debug)]
pub struct StarData {
    pub crd: ObjEqCoord,
    pub mag: ObjMagnitude,
    pub bv:  StarBV,
}

#[derive(Clone, Debug)]
pub struct NamedStar {
    pub cnst_id:  u8,
    pub name:     String,
    pub name_lc:  String,
    pub bayer:    String,
    pub bayer_lc: String,
    pub data:     StarData,
}

#[derive(Clone)]
pub struct Star {
    pub data: StarData,
}

#[derive(Clone)]
pub struct StarZone {
    coords: [EqCoord; 4],
    stars:  Vec<Star>,
    nstars: Vec<NamedStar>,
}

impl StarZone {
    pub fn coords(&self) -> &[EqCoord; 4] {
        &self.coords
    }

    pub fn stars(&self) -> &Vec<Star> {
        &self.stars
    }

    pub fn named_stars(&self) -> &Vec<NamedStar> {
        &self.nstars
    }
}

#[derive(Hash, PartialEq, Eq, Clone, Copy)]
pub struct SkyZoneKey {
    ra_key: u16,
    dec_key: u16,
}

impl SkyZoneKey {
    pub const RA_COUNT: usize = 40;
    pub const DEC_COUNT: usize = 40;

    pub fn from_indices(ra_key: u16, dec_key: u16) -> Self {
        Self { ra_key, dec_key }
    }

    pub fn from_coord(mut ra: f64, mut dec: f64) -> Self {
        while ra >= 2.0 * PI { ra -= 2.0 * PI; }
        while ra <= 0.0 { ra += 2.0 * PI; }
        dec += 0.5 * PI;
        dec = dec.clamp(0.0, PI);
        let ra_int = (Self::RA_COUNT as f64 * ra / (2.0 * PI)) as u16;
        let dec_int = (Self::DEC_COUNT as f64 * dec / PI) as u16;
        SkyZoneKey {
            ra_key: ra_int,
            dec_key: dec_int,
        }
    }

    pub fn to_coords(&self) -> [EqCoord; 4] {
        let zone_ra_key_to_value = |ra_int: u16| -> f64 {
            2.0 * PI * ra_int as f64 / SkyZoneKey::RA_COUNT as f64
        };
        let zone_dec_key_to_value = |dec_int: u16| -> f64 {
            PI * dec_int as f64 / SkyZoneKey::DEC_COUNT as f64 - 0.5 * PI
        };
        let ra1 = zone_ra_key_to_value(self.ra_key);
        let ra2 = zone_ra_key_to_value(self.ra_key+1);
        let dec1 = zone_dec_key_to_value(self.dec_key);
        let dec2 = zone_dec_key_to_value(self.dec_key+1);
        [
            EqCoord {ra: ra1, dec: dec1},
            EqCoord {ra: ra2, dec: dec1},
            EqCoord {ra: ra2, dec: dec2},
            EqCoord {ra: ra1, dec: dec2},
        ]
    }
}

pub struct Stars {
    zones: HashMap<SkyZoneKey, StarZone>,
}

impl Stars {
    pub fn new() -> Self {
        Self {
            zones: HashMap::new(),
        }
    }

    pub fn zones(&self) -> &HashMap<SkyZoneKey, StarZone> {
        &self.zones
    }

    pub fn add_star(&mut self, data: StarData, name: Option<String>, bayer: Option<String>, cnst_id: Option<u8>) {
        let ra = data.crd.ra();
        let dec = data.crd.dec();
        let key = SkyZoneKey::from_coord(ra, dec);
        let zone = if let Some(zone) = self.zones.get_mut(&key) {
            zone
        } else {
            let new_zone = StarZone {
                coords: key.to_coords(),
                stars: Vec::new(),
                nstars: Vec::new(),
            };
            self.zones.insert(key, new_zone);
            self.zones.get_mut(&key).unwrap()
        };

        if let (Some(name), Some(bayer), Some(cnst_id)) = (name, bayer, cnst_id) {
            let name_lc = name.to_lowercase();
            let bayer_lc = bayer.to_lowercase();
            let star = NamedStar{ data, name, name_lc, bayer, bayer_lc, cnst_id };
            zone.nstars.push(star);
        } else {
            zone.stars.push(Star { data });
        }
    }

    pub fn get_nearest(&self, crd: &EqCoord, max_mag: f32) -> Option<(NamedStar, f64)> {
        let max_mag = ObjMagnitude::new(max_mag);

        let nearest = self.zones.iter()
            .flat_map(|(_, zone)| &zone.stars)
            .filter(|star| star.data.mag < max_mag)
            .map(|star| (star, EqCoord::angle_between(&star.data.crd.to_eq(), crd)))
            .min_by(|(_, angle1), (_, angle2)| f64::total_cmp(angle1, angle2));

        let nearest_named = self.zones.iter()
            .flat_map(|(_, zone)| &zone.nstars)
            .filter(|star| star.data.mag < max_mag)
            .map(|star| (star, EqCoord::angle_between(&star.data.crd.to_eq(), crd)))
            .min_by(|(_, angle1), (_, angle2)| f64::total_cmp(angle1, angle2));

        let star_to_named_star = |star: &Star| -> NamedStar {
            NamedStar {
                data:     star.data.clone(),
                name:     String::new(),
                name_lc:  String::new(),
                bayer:    String::new(),
                bayer_lc: String::new(),
                cnst_id:  0,
            }
        };

        if let (Some((nearest, angle)), Some((nearest_named, named_angle)))
        = (nearest, nearest_named) {
            if angle < named_angle {
                let named_star = star_to_named_star(nearest);
                return Some((named_star, angle))
            } else {
                 return Some((nearest_named.clone(), named_angle))
            }
        } else if let Some((nearest, angle)) = nearest {
            let named_star = star_to_named_star(nearest);
            return Some((named_star, angle))
        } else if let Some((nearest, named_angle)) = nearest_named {
            return Some((nearest.clone(), named_angle))
        }

        None
    }

    fn find(&self, text: &str, mode: SearchMode) -> Vec<SkymapObject> {
        let mut result = Vec::new();
        for star in self.zones.iter().flat_map(|(_, zone)| &zone.nstars) {
            let found = match mode {
                SearchMode::StartWith =>
                    star.name_lc.starts_with(text) ||
                    star.bayer_lc.starts_with(text),
                SearchMode::Contains =>
                    star.name_lc.contains(text) ||
                    star.bayer_lc.contains(text),
            };
            if found {
                result.push(SkymapObject::Star(star.clone()));
            }
        }
        result
    }

}

bitflags! {
    #[derive(Clone, Serialize, Deserialize)]
    #[serde(default)]
    pub struct ItemsToShow: u32 {
        const STARS    = 1 << 0;
        const DSO      = 1 << 1;
        const OUTLINES = 1 << 2;
        const CLUSTERS = 1 << 3;
        const NEBULAS  = 1 << 4;
        const GALAXIES = 1 << 5;
    }
}

impl Default for ItemsToShow {
    fn default() -> Self {
        Self::all()
    }
}

#[derive(Debug, Clone, Copy)]
pub enum SkyItemType {
    None,
    Star,
    DoubleStar,
    Galaxy,
    StarCluster,
    PlanetaryNebula,
    DarkNebula,
    EmissionNebula,
    Nebula,
    ReflectionNebula,
    HIIIonizedRegion,
    SupernovaRemnant,
    GalaxyPair,
    GalaxyTriplet,
    GroupOfGalaxies,
    AssociationOfStars,
    StarClusterAndNebula,
}

impl SkyItemType {
    fn from_str(name: &str) -> Option<Self> {
        let type_is = |col_name| name.eq_ignore_ascii_case(col_name);
        if      type_is("g")      { Some(SkyItemType::Galaxy) }
        else if type_is("cl")     { Some(SkyItemType::StarCluster) }
        else if type_is("pn")     { Some(SkyItemType::PlanetaryNebula) }
        else if type_is("drkn")   { Some(SkyItemType::DarkNebula) }
        else if type_is("emn")    { Some(SkyItemType::EmissionNebula) }
        else if type_is("neb")    { Some(SkyItemType::Nebula) }
        else if type_is("rfn")    { Some(SkyItemType::ReflectionNebula) }
        else if type_is("hii")    { Some(SkyItemType::HIIIonizedRegion) }
        else if type_is("snr")    { Some(SkyItemType::SupernovaRemnant) }
        else if type_is("gpair")  { Some(SkyItemType::GalaxyPair) }
        else if type_is("gtrpl")  { Some(SkyItemType::GalaxyTriplet) }
        else if type_is("ggroup") { Some(SkyItemType::GroupOfGalaxies) }
        else if type_is("*ass")   { Some(SkyItemType::AssociationOfStars) }
        else if type_is("cl+n")   { Some(SkyItemType::StarClusterAndNebula) }
        else                      { None }
    }

    pub fn test_filter_flag(self, flags: &ItemsToShow) -> bool {
        use SkyItemType::*;
        match self {
            Star | DoubleStar =>
                flags.contains(ItemsToShow::STARS),
            Galaxy | GalaxyPair | GalaxyTriplet | GroupOfGalaxies =>
                flags.contains(ItemsToShow::GALAXIES) && flags.contains(ItemsToShow::DSO),
            StarCluster | AssociationOfStars =>
                flags.contains(ItemsToShow::CLUSTERS) && flags.contains(ItemsToShow::DSO),
            PlanetaryNebula | DarkNebula | EmissionNebula | Nebula |
            ReflectionNebula | SupernovaRemnant | HIIIonizedRegion =>
                flags.contains(ItemsToShow::NEBULAS) && flags.contains(ItemsToShow::DSO),
            StarClusterAndNebula =>
                flags.contains(ItemsToShow::NEBULAS) && flags.contains(ItemsToShow::DSO) ||
                flags.contains(ItemsToShow::CLUSTERS) && flags.contains(ItemsToShow::DSO),
            _ => false,
        }
    }
}

#[derive(Clone)]
pub struct Outline {
    pub name:    String,
    pub polygon: Vec<ObjEqCoord>
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
enum DsoNamePart {
    Text(String),
    Value(u32),
}

type NameParts = Vec<DsoNamePart>;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct DsoName{
    orig_text: String,
    parts:  NameParts,
}

impl DsoName {
    fn from_str(text: &str) -> Self {
        let mut part = String::new();
        let mut parts = Vec::new();
        let add_part = move |part: &mut String, parts: &mut Vec<DsoNamePart>| {
            let part_trimmed = part.trim();
            if part_trimmed.is_empty() { return }
            if let Ok(value) = part_trimmed.parse::<u32>() {
                parts.push(DsoNamePart::Value(value));
            } else {
                parts.push(DsoNamePart::Text(part_trimmed.to_lowercase()));
            }
            part.clear();
        };
        let mut is_numeric = false;
        for char in text.chars() {
            if char.is_whitespace() || char == '-' {
                add_part(&mut part, &mut parts);
            } else if is_numeric != char.is_numeric() {
                is_numeric = char.is_numeric();
                add_part(&mut part, &mut parts);
                part.push(char);
            } else {
                part.push(char);
            }
        }
        add_part(&mut part, &mut parts);
        Self {
            orig_text: text.to_string(),
            parts,
        }
    }

    pub fn text(&self) -> &str {
        &self.orig_text
    }
}

#[test]
fn test_dso_name() {
    let name = DsoName::from_str("Test-1aaaa42 2 3");
    assert_eq!(name.parts[0], DsoNamePart::Text("test".to_string()));
    assert_eq!(name.parts[1], DsoNamePart::Value(1));
    assert_eq!(name.parts[2], DsoNamePart::Text("aaaa".to_string()));
    assert_eq!(name.parts[3], DsoNamePart::Value(42));
    assert_eq!(name.parts[4], DsoNamePart::Value(2));
    assert_eq!(name.parts[5], DsoNamePart::Value(3));
}

#[derive(Debug, Clone)]
pub struct DsoNickName {
    orig: String,
    lc: String,
}

impl DsoNickName {
    pub fn text(&self) -> &str {
        &self.orig
    }
}

#[derive(Debug, Clone)]
pub struct DsoItem {
    pub names:     Vec<DsoName>,
    pub nicknames: Vec<DsoNickName>,
    pub crd:       ObjEqCoord,
    pub mag_v:     Option<ObjMagnitude>,
    pub mag_b:     Option<ObjMagnitude>,
    pub cnst_id:   u8,
    pub obj_type:  SkyItemType,
    pub maj_axis:  Option<f32>,
    pub min_axis:  Option<f32>,
    pub angle:     Option<f32>,
}

impl DsoItem {
    pub fn any_magnitude(&self) -> Option<ObjMagnitude> {
        if self.mag_v.is_some() {
            self.mag_v
        } else {
            self.mag_b
        }
    }
}

#[derive(Debug, Clone)]
pub enum SkymapObject {
    Star(NamedStar),
    Dso(DsoItem),
}

impl SkymapObject {
    pub fn names(&self) -> Vec<&str> {
        match self {
            Self::Dso(dso) => {
                dso.names.iter().map(|n| n.orig_text.as_str()).collect()
            }
            Self::Star(star) => {
                let mut result = Vec::new();
                if !star.name.is_empty() {
                    result.push(star.name.as_str());
                }
                if !star.bayer.is_empty() {
                    result.push(star.bayer.as_str());
                }
                result
            }
        }
    }

    pub fn nicknames(&self) -> Vec<&str> {
        match self {
            Self::Dso(dso) =>
                dso.nicknames.iter().map(|n| n.orig.as_str()).collect(),
            Self::Star(_) => vec![],
        }
    }

    pub fn obj_type(&self) -> SkyItemType {
        match self {
            Self::Dso(dso) => dso.obj_type,
            Self::Star(_) => SkyItemType::Star,
        }
    }

    pub fn crd(&self) -> EqCoord {
        match self {
            Self::Dso(dso) => dso.crd.to_eq(),
            Self::Star(star) => star.data.crd.to_eq(),
        }
    }

    pub fn mag_v(&self) -> Option<f32> {
        match self {
            Self::Dso(dso) => dso.mag_v.map(|mag| mag.get()),
            Self::Star(star) => Some(star.data.mag.get()),
        }
    }

    pub fn mag_b(&self) -> Option<f32> {
        match self {
            Self::Dso(dso) => dso.mag_b.map(|mag| mag.get()),
            Self::Star(_) => None,
        }
    }

    pub fn bv(&self) -> Option<f32> {
        match self {
            Self::Dso(dso) => {
                if let (Some(mag_v), Some(mag_b)) = (dso.mag_v, dso.mag_b) {
                    Some(mag_b.get() - mag_v.get())
                } else {
                    None
                }
            },
            Self::Star(star) => Some(star.data.bv.get()),
        }
    }

}

pub struct SkyMap {
    constellations:   HashMap<u8, &'static str>,
    const_id_by_name: HashMap<&'static str, u8>,
    stars:            Stars,
    objects:          Vec<DsoItem>,
    outlines:         Vec<Outline>,
}

impl SkyMap {
    pub fn new() -> Self {
        let const_data = [
            ( 1, "hya"), ( 2, "vir"), ( 3, "uma"), ( 4, "cet"),
            ( 5, "her"), ( 6, "eri"), ( 7, "peg"), ( 8, "dra"),
            ( 9, "cen"), (10, "aqr"), (11, "oph"), (12, "leo"),
            (13, "boo"), (14, "psc"), (15, "sgr"), (16, "cyg"),
            (17, "tau"), (18, "cam"), (19, "and"), (20, "pup"),
            (21, "aur"), (22, "aql"), (23, "ser"), (24, "per"),
            (25, "cas"), (26, "ori"), (27, "cep"), (28, "lyn"),
            (29, "lib"), (30, "gem"), (31, "cnc"), (32, "vel"),
            (33, "sco"), (34, "car"), (35, "mon"), (36, "scl"),
            (37, "phe"), (38, "cvn"), (39, "ari"), (40, "cap"),
            (41, "for"), (42, "com"), (43, "cma"), (44, "pav"),
            (45, "gru"), (46, "lup"), (47, "sex"), (48, "tuc"),
            (49, "ind"), (50, "oct"), (51, "lep"), (52, "lyr"),
            (53, "crt"), (54, "col"), (55, "vul"), (56, "umi"),
            (57, "tel"), (58, "hor"), (59, "pic"), (60, "psa"),
            (61, "hyi"), (62, "ant"), (63, "ara"), (64, "lmi"),
            (65, "pyx"), (66, "mic"), (67, "aps"), (68, "lac"),
            (69, "del"), (70, "crv"), (71, "cmi"), (72, "dor"),
            (73, "crb"), (74, "nor"), (75, "men"), (76, "vol"),
            (77, "mus"), (78, "tri"), (79, "cha"), (80, "cra"),
            (81, "cae"), (82, "ret"), (83, "tra"), (84, "sct"),
            (85, "cir"), (86, "sge"), (87, "equ"), (88, "cru"),
        ];
        let constellations = HashMap::from(const_data);
        let const_id_by_name = const_data.into_iter().map(|(id, name)| (name, id)).collect();

        Self {
            constellations,
            const_id_by_name,
            stars:           Stars::new(),
            objects:         Vec::new(),
            outlines:        Vec::new(),
        }
    }

    pub fn objects(&self) -> &Vec<DsoItem> {
        &self.objects
    }

    pub fn outlines(&self) -> &Vec<Outline> {
        &self.outlines
    }

    pub fn stars(&self) -> &Stars {
        &self.stars
    }

    pub fn merge_other_skymaps(&mut self, other: &Self) {
        self.objects.extend_from_slice(&other.objects);
        self.outlines.extend_from_slice(&other.outlines);

        for (key, star_zone) in &other.stars.zones {
            self.stars.zones.entry(*key)
                .and_modify(|existing| {
                    existing.stars.extend_from_slice(&star_zone.stars);
                    existing.nstars.extend_from_slice(&star_zone.nstars);
                })
                .or_insert(star_zone.clone());
        }
    }

    pub fn load_dso(&mut self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let mut rdr = csv::ReaderBuilder::new()
            .delimiter(b';')
            .from_path(path)?;
        let headers = rdr.headers()?;
        let find_col = |name| -> anyhow::Result<usize> {
            headers.iter()
                .position(|c| c.eq_ignore_ascii_case(name))
                .ok_or_else(|| anyhow::anyhow!("`{}` col not found", name))
        };
        let type_col      = find_col("type")?;
        let ra_col        = find_col("ra")?;
        let dec_col       = find_col("dec")?;
        let const_col     = find_col("constellation")?;
        let names_col     = find_col("names")?;
        let nicknames_col = find_col("nicknames")?;
        let mag_v_col     = find_col("mag_v")?;
        let mag_b_col     = find_col("mag_b")?;
        let maj_axis_col  = find_col("major_axis")?;
        let min_axis_col  = find_col("minor_axis")?;
        let angle_col     = find_col("angle")?;

        for record in rdr.records().filter_map(|record| record.ok()) {
            if record.is_empty() { continue; }
            let Some(obj_type) = SkyItemType::from_str(record[type_col].trim()) else { continue; };
            let Some(ra) = sexagesimal_to_value(record[ra_col].trim()) else { continue; };
            let Some(dec) = sexagesimal_to_value(record[dec_col].trim()) else { continue; };
            let cnst_id = *self.const_id_by_name.get(record[const_col].trim()).unwrap_or(&0);
            let names_str = record[names_col].trim();
            let nicknames_str = record[nicknames_col].trim();
            let mag_v = record[mag_v_col].trim().parse().ok();
            let mag_b = record[mag_b_col].trim().parse().ok();
            let maj_axis = record[maj_axis_col].trim().parse().ok();
            let min_axis = record[min_axis_col].trim().parse().ok();
            let angle = record[angle_col].trim().parse().ok().map(|v| degree_to_radian(v) as f32);
            let mut names = Vec::new();
            for name in names_str.split("|").filter(|name| !name.is_empty()) {
                names.push(DsoName::from_str(name))
            }
            let nicknames = nicknames_str
                .split("|")
                .map(str::trim)
                .map(|s| DsoNickName {orig: s.to_string(), lc: s.to_lowercase()} )
                .collect::<Vec<_>>();
            let crd = ObjEqCoord::new(
                hour_to_radian(ra),
                degree_to_radian(dec)
            );
            let mag_v = mag_v.map(ObjMagnitude::new);
            let mag_b = mag_b.map(ObjMagnitude::new);
            let object = DsoItem {
                names, nicknames, crd, mag_v, mag_b, cnst_id,
                obj_type, maj_axis, min_axis, angle
            };
            self.objects.push(object);
        }
        Ok(())
    }

    pub fn load_stars(&mut self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let file = std::fs::File::open(path)?;
        let mut buffer = std::io::BufReader::new(file);

        // file header

        let mut header_buf = [0_u8; 15];
        buffer.read_exact(&mut header_buf)?;
        if &header_buf != b"astralite-stars" {
            anyhow::bail!("Not a astralite stars file");
        }

        // file version

        let mut buf_u16 = [0_u8; 2];
        buffer.read_exact(&mut buf_u16)?;
        let version = u16::from_be_bytes(buf_u16);
        if version != 1 {
            anyhow::bail!("File version {} is not supported", version);
        }

        // stars count

        let mut buf_usize = [0_u8; std::mem::size_of::<usize>()];
        buffer.read_exact(&mut buf_usize)?;
        let stars_count = usize::from_be_bytes(buf_usize);

        // stars data

        let mut bit_reader = BitReader::endian(&mut buffer, BigEndian);
        let mut ra_decompressor = ValuesDecompressor::new();
        let mut dec_decompressor = ValuesDecompressor::new();
        let mut mag_decompressor = ValuesDecompressor::new();
        let mut bv_decompressor = ValuesDecompressor::new();

        for _ in 0..stars_count {
            let ra = ra_decompressor.read_u32(&mut bit_reader)?;
            let dec = dec_decompressor.read_i32(&mut bit_reader)?;
            let mag = mag_decompressor.read_i32(&mut bit_reader)? as f64 / 100.0;
            let bv = bv_decompressor.read_i32(&mut bit_reader)? as f64 / 100.0;
            let star_data = StarData {
                crd: ObjEqCoord::new_from_int(ra, dec),
                mag: ObjMagnitude::new(mag as f32),
                bv:  StarBV::new(bv as f32),
            };
            self.stars.add_star(star_data, None, None, None);
        }

        Ok(())
    }

    pub fn load_named_stars(&mut self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let mut rdr = csv::ReaderBuilder::new()
            .delimiter(b';')
            .from_path(path)?;
        let headers = rdr.headers()?;
        let find_col = |name| -> anyhow::Result<usize> {
            headers.iter()
                .position(|c| c.eq_ignore_ascii_case(name))
                .ok_or_else(|| anyhow::anyhow!("`{}` col not found", name))
        };
        let name_col  = find_col("name")?;
        let bayer_col = find_col("bayer")?;
        let ra_col    = find_col("ra")?;
        let dec_col   = find_col("dec")?;
        let const_col = find_col("constellation")?;
        let mag_col   = find_col("mag")?;
        let bv_col    = find_col("bv")?;
        for record in rdr.records().filter_map(|record| record.ok()) {
            if record.is_empty() { continue; }
            let name = record[name_col].trim();
            let bayer = record[bayer_col].trim();
            let Some(ra_hours) = sexagesimal_to_value(record[ra_col].trim()) else { continue; };
            let Some(dec_degrees) = sexagesimal_to_value(record[dec_col].trim()) else { continue; };
            let cnst_id = *self.const_id_by_name.get(record[const_col].trim()).unwrap_or(&0);
            let magnitude = record[mag_col].trim().parse().unwrap_or(f32::NAN);
            let bv = record[bv_col].trim().parse().unwrap_or(f32::NAN);
            let ra = hour_to_radian(ra_hours);
            let dec = degree_to_radian(dec_degrees);
            let star_data = StarData {
                crd: ObjEqCoord::new(ra, dec),
                mag: ObjMagnitude::new(magnitude),
                bv:  StarBV::new(bv),
            };

            self.stars.add_star(
                star_data,
                Some(name.to_string()),
                Some(bayer.to_string()),
                Some(cnst_id)
            );
        }
        Ok(())
    }

    pub fn load_stellarium_outlines_file(&mut self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let file = std::fs::File::open(path)?;
        let reader = std::io::BufReader::new(file);
        let mut name = String::new();
        let mut points = Vec::new();
        for line in reader.lines().map_while(Result::ok) {
            let line_items = line
                .split(' ')
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>();
            let parse_coords = |ra_str: &str, dec_str: &str| -> anyhow::Result<(f64, f64)> {
                let ra = hour_to_radian( ra_str.parse::<f64>()?);
                let dec = hour_to_radian(dec_str.parse::<f64>()?);
                Ok((ra, dec))
            };
            match *line_items.as_slice() {
                [ra_str, dec_str, "start", item_name] => {
                    name = item_name.to_string();
                    points.clear();
                    let (ra, dec) = parse_coords(ra_str, dec_str)?;
                    points.push(ObjEqCoord::new(ra, dec));
                },
                [ra_str, dec_str, "end"] => {
                    let (ra, dec) = parse_coords(ra_str, dec_str)?;
                    points.push(ObjEqCoord::new(ra, dec));
                    self.outlines.push(Outline{
                        name: std::mem::take(&mut name),
                        polygon: std::mem::take(&mut points),
                    });
                }
                [ra_str, dec_str, "vertex"] => {
                    let (ra, dec) = parse_coords(ra_str, dec_str)?;
                    points.push(ObjEqCoord::new(ra, dec));
                },
                _ => {
                    eprintln!("Strange outline record: {}", line);
                },
            }
        }
        Ok(())
    }

    pub fn get_nearest(
        &self,
        crd:          &EqCoord,
        max_dso_mag:  f32,
        max_star_mag: f32,
        filter:       &ItemsToShow,
    ) -> Option<SkymapObject> {
        let nearest_star = if filter.contains(ItemsToShow::STARS) {
            self.stars.get_nearest(crd, max_star_mag)
        } else {
            None
        };
        let nearest_obj = self.get_nearest_dso_object(crd, max_dso_mag, filter);
        match (nearest_star, nearest_obj) {
            (Some((star, star_angle)), Some((obj, obj_angle))) => {
                if star_angle < obj_angle {
                    Some(SkymapObject::Star(star))
                } else {
                    Some(SkymapObject::Dso(obj))
                }
            }
            (Some((star, _)), None) =>
                Some(SkymapObject::Star(star)),
            (None, Some((obj, _))) =>
                Some(SkymapObject::Dso(obj)),
            _ =>
                None
        }
    }

    pub fn get_nearest_dso_object(
        &self,
        crd:         &EqCoord,
        max_dso_mag: f32,
        filter:      &ItemsToShow,
    ) -> Option<(DsoItem, f64)> {
        let max_mag = ObjMagnitude::new(max_dso_mag);
        let nearest_obj = self.objects.iter()
            .filter(|obj| obj.obj_type.test_filter_flag(filter))
            .filter(|obj| obj.any_magnitude().map(|mag| mag <= max_mag).unwrap_or(false))
            .map(|obj| (obj, EqCoord::angle_between(&obj.crd.to_eq(), crd)))
            .min_by(|(_, angle1), (_, angle2)| f64::total_cmp(angle1, angle2));
        nearest_obj.map(|(obj, angle)| (obj.clone(), angle))
    }

    pub fn search(&self, text: &str) -> Vec<SkymapObject> {
        let mut result = Vec::new();
        let text_lc = text.trim().to_lowercase();
        if text_lc.is_empty() {
            return result;
        }
        let mut uniq_names = HashSet::new();
        let mut apdate_result = |items: Vec<SkymapObject>| {
            for item in items {
                let names = item.names().join("|");
                if uniq_names.contains(&names) { continue; }
                result.push(item);
                uniq_names.insert(names);
            }
        };
        apdate_result(self.find(&text_lc, SearchMode::StartWith));
        apdate_result(self.stars.find(&text_lc, SearchMode::StartWith));
        apdate_result(self.find(&text_lc, SearchMode::Contains));
        apdate_result(self.stars.find(&text_lc, SearchMode::Contains));
        result
    }

    fn find(&self, text_lc: &str, mode: SearchMode) -> Vec<SkymapObject> {
        let name_to_search = DsoName::from_str(text_lc);
        let mut result = Vec::new();
        for item in &self.objects {
            let mut matched = false;
            for name in &item.names {
                matched |= match mode {
                    SearchMode::StartWith =>
                        name.orig_text.starts_with(text_lc) ||
                        name.parts == name_to_search.parts,
                    SearchMode::Contains =>
                        name.orig_text.contains(text_lc),
                };
            }

            for nickname in &item.nicknames {
                matched |= match mode {
                    SearchMode::StartWith =>
                        nickname.lc.starts_with(text_lc),
                    SearchMode::Contains =>
                        nickname.lc.contains(text_lc),
                };
            }

            if matched {
                result.push(SkymapObject::Dso(item.clone()));
            }
        }
        result
    }
}
