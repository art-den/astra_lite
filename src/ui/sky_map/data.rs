use std::{collections::*, f64::consts::PI, fmt::Debug, io::{BufRead, Read}, path::Path};
use std::f32::consts::PI as PI_f32;
use bitstream_io::{BigEndian, BitReader};
use crate::{indi::sexagesimal_to_value, utils::compression::ValuesDecompressor};
use super::utils::*;


const ID_CAT_OTHER:   u16 = 0;
const ID_CAT_MESSIER: u16 = 1;
const ID_CAT_NGC:     u16 = 2;
const ID_CAT_IC:      u16 = 3;

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

    pub fn to_eq(&self) -> EqCoord {
        EqCoord {
            ra: self.ra(),
            dec: self.dec(),
        }
    }

    pub fn new_from_int(ra: u32, dec: i32) -> Self {
        Self { ra, dec }
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
    let obj = ObjEqCoord::new(24.0, 90.0);
    dbg!(&obj);
    assert_eq!(obj.ra(), 24.0);
    assert_eq!(obj.dec(), 90.0);
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

#[derive(Clone, Debug)]
pub struct StarData {
    pub crd: ObjEqCoord,
    pub mag: ObjMagnitude,
    pub bv:  StarBV,
}

#[derive(Clone, Debug)]
pub struct NamedStar {
    pub cnst_id: u8,
    pub name:    String,
    pub bayer:   String,
    pub data:    StarData,
}

#[derive(Clone)]
pub struct Star {
    pub data: StarData,
}

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

pub type StarZoneKey = (u16, u16);

pub struct Stars {
    zones: HashMap<StarZoneKey, StarZone>,
}

impl Stars {
    const RA_COUNT: usize = 40;
    const DEC_COUNT: usize = 40;

    pub fn new() -> Self {
        Self {
            zones: HashMap::new(),
        }
    }

    pub fn zones(&self) -> &HashMap<StarZoneKey, StarZone> {
        &self.zones
    }

    pub fn add_star(&mut self, data: StarData, name: Option<String>, bayer: Option<String>, cnst_id: Option<u8>) {
        let zone_ra_key_to_value = |ra_int: u16| -> f64 {
            2.0 * PI * ra_int as f64 / Self::RA_COUNT as f64
        };

        let zone_dec_key_to_value = |dec_int: u16| -> f64 {
            PI * dec_int as f64 / Self::DEC_COUNT as f64 - 0.5 * PI
        };

        let ra = data.crd.ra();
        let dec = data.crd.dec();
        let key = Self::get_key_for_coord(ra, dec);
        let zone = if let Some(zone) = self.zones.get_mut(&key) {
            zone
        } else {
            let ra1 = zone_ra_key_to_value(key.0);
            let ra2 = zone_ra_key_to_value(key.0+1);
            let dec1 = zone_dec_key_to_value(key.1);
            let dec2 = zone_dec_key_to_value(key.1+1);
            let new_zone = StarZone {
                coords: [
                    EqCoord {ra: ra1, dec: dec1},
                    EqCoord {ra: ra2, dec: dec1},
                    EqCoord {ra: ra2, dec: dec2},
                    EqCoord {ra: ra1, dec: dec2},
                ],
                stars: Vec::new(),
                nstars: Vec::new(),
            };
            self.zones.insert(key, new_zone);
            self.zones.get_mut(&key).unwrap()
        };

        if let (Some(name), Some(bayer), Some(cnst_id)) = (name, bayer, cnst_id) {
            zone.nstars.push(NamedStar {
                data,
                name,
                bayer,
                cnst_id
            });
        } else {
            zone.stars.push(Star { data });
        }
    }

    pub fn get_key_for_coord(mut ra: f64, mut dec: f64) -> StarZoneKey {
        while ra >= 2.0 * PI { ra -= 2.0 * PI; }
        while ra <= 0.0 { ra += 2.0 * PI; }
        dec += 0.5*PI;
        if dec > PI { dec = PI; }
        if dec < 0.0 { dec = 0.0; }
        let ra_int = (Self::RA_COUNT as f64 * ra / (2.0 * PI)) as u16;
        let dec_int = (Self::DEC_COUNT as f64 * dec / PI) as u16;
        (ra_int as u16, dec_int as u16)
    }

    pub fn get_nearest(&self, crd: &EqCoord, max_mag: f32) -> Option<(NamedStar, f64)> {
        let max_mag = ObjMagnitude::new(max_mag);

        let nearest = self.zones.iter()
            .flat_map(|(_, zone)| &zone.stars)
            .filter(|star| star.data.mag < max_mag)
            .map(|star| (star, EqCoord::angle_between(&star.data.crd.to_eq(), crd)))
            .min_by(|(_, angle1), (_, angle2)| f64::total_cmp(&angle1, &angle2));

        let nearest_named = self.zones.iter()
            .flat_map(|(_, zone)| &zone.nstars)
            .filter(|star| star.data.mag < max_mag)
            .map(|star| (star, EqCoord::angle_between(&star.data.crd.to_eq(), crd)))
            .min_by(|(_, angle1), (_, angle2)| f64::total_cmp(&angle1, &angle2));

        let star_to_named_star = |star: &Star| -> NamedStar {
            NamedStar {
                data:    star.data.clone(),
                bayer:   String::new(),
                cnst_id: 0,
                name:    String::new(),
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
}

#[derive(Debug, Clone, Copy)]
pub enum DsoType {
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

impl DsoType {
    fn from_str(name: &str) -> Option<Self> {
        let type_is = |col_name| name.eq_ignore_ascii_case(col_name);
        if      type_is("g")      { Some(DsoType::Galaxy) }
        else if type_is("cl")     { Some(DsoType::StarCluster) }
        else if type_is("pn")     { Some(DsoType::PlanetaryNebula) }
        else if type_is("drkn")   { Some(DsoType::DarkNebula) }
        else if type_is("emn")    { Some(DsoType::EmissionNebula) }
        else if type_is("neb")    { Some(DsoType::Nebula) }
        else if type_is("rfn")    { Some(DsoType::ReflectionNebula) }
        else if type_is("hii")    { Some(DsoType::HIIIonizedRegion) }
        else if type_is("snr")    { Some(DsoType::SupernovaRemnant) }
        else if type_is("gpair")  { Some(DsoType::GalaxyPair) }
        else if type_is("gtrpl")  { Some(DsoType::GalaxyTriplet) }
        else if type_is("ggroup") { Some(DsoType::GroupOfGalaxies) }
        else if type_is("*ass")   { Some(DsoType::AssociationOfStars) }
        else if type_is("cl+n")   { Some(DsoType::StarClusterAndNebula) }
        else                      { None }
    }
}

pub struct Outline {
    pub name:    String,
    pub polygon: Vec<ObjEqCoord>
}

#[derive(Debug, Clone)]
pub struct DsoName {
    pub catalogue: u16,
    pub name:      String,
}

#[derive(Debug, Clone)]
pub struct DsoItem {
    pub names:    Vec<DsoName>,
    pub crd:      ObjEqCoord,
    pub mag:      ObjMagnitude,
    pub cnst_id:  u8,
    pub obj_type: DsoType,
    pub maj_axis: Option<f32>,
    pub min_axis: Option<f32>,
    pub angle:    Option<f32>,
}

#[derive(Debug)]
pub enum Object {
    Star(NamedStar),
    Dso(DsoItem),
}

impl Object {
    pub fn crd(&self) -> EqCoord {
        match self {
            Self::Dso(dso) => dso.crd.to_eq(),
            Self::Star(star) => star.data.crd.to_eq(),
        }
    }
}

pub struct SkyMap {
    catalogue_by_id:  HashMap<u16, String>,
    constellations:   HashMap<u8, &'static str>,
    const_id_by_name: HashMap<&'static str, u8>,
    named_stars:      Vec<NamedStar>,
    stars:            Stars,
    objects:          Vec<DsoItem>,
    obj_idx_by_name:  HashMap<String, usize>,
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

        let catalogue_by_id = HashMap::from([
            (ID_CAT_OTHER, String::new()),
            (ID_CAT_MESSIER, "Messier".to_string()),
            (ID_CAT_NGC,     "NGC".to_string()),
            (ID_CAT_IC,      "IC".to_string()),
        ]);

        Self {
            catalogue_by_id,
            constellations,
            const_id_by_name,
            named_stars:     Vec::new(),
            stars:           Stars::new(),
            objects:         Vec::new(),
            obj_idx_by_name: HashMap::new(),
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
        let type_col     = find_col("type")?;
        let ra_col       = find_col("ra")?;
        let dec_col      = find_col("dec")?;
        let const_col    = find_col("constellation")?;
        let messier_col  = find_col("messier")?;
        let ngc_col      = find_col("ngc")?;
        let ic_col       = find_col("ic")?;
        let other_col    = find_col("other")?;
        let mag_col      = find_col("mag")?;
        let maj_axis_col = find_col("major_axis")?;
        let min_axis_col = find_col("minor_axis")?;
        let angle_col    = find_col("angle")?;

        for record in rdr.records().filter_map(|record| record.ok()) {
            if record.is_empty() { continue; }
            let Some(obj_type) = DsoType::from_str(record[type_col].trim()) else { continue; };
            let Some(ra) = sexagesimal_to_value(record[ra_col].trim()) else { continue; };
            let Some(dec) = sexagesimal_to_value(record[dec_col].trim()) else { continue; };
            let cnst_id = *self.const_id_by_name.get(record[const_col].trim()).unwrap_or(&0);
            let magnitude = record[mag_col].trim().parse().unwrap_or(f32::NAN);
            let maj_axis = record[maj_axis_col].trim().parse().ok();
            let min_axis = record[min_axis_col].trim().parse().ok();
            let angle = record[angle_col].trim().parse().ok().map(|v: f32| v * PI_f32 / 180.0);
            let mut names = Vec::new();
            fn append_names (col_str: &str, names: &mut Vec<DsoName>, catalogue: u16) {
                for name in col_str.split("|").filter(|name| !name.is_empty()) {
                    names.push(DsoName {
                        catalogue,
                        name: name.to_string(),
                    })
                }
            }
            append_names(record[messier_col].trim(), &mut names, ID_CAT_MESSIER);
            append_names(record[ngc_col].trim(), &mut names, ID_CAT_NGC);
            append_names(record[ic_col].trim(), &mut names, ID_CAT_IC);
            append_names(record[other_col].trim(), &mut names, ID_CAT_OTHER);
            let crd = ObjEqCoord::new(
                2.0 * PI * ra / 24.0,
                2.0 * PI * dec / 360.0
            );
            let mag = ObjMagnitude::new(magnitude);
            let object = DsoItem {
                names, crd, mag, cnst_id,
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
            let ra = 2.0 * PI * ra_hours / 24.0;
            let dec = 2.0 * PI * dec_degrees / 360.0;
            let star_data = StarData {
                crd: ObjEqCoord::new(ra, dec),
                mag: ObjMagnitude::new(magnitude as f32),
                bv:  StarBV::new(bv as f32),
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
        for line in reader.lines().filter_map(|line| line.ok()) {
            let line_items = line
                .split(' ')
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>();
            let parse_coords = |ra_str: &str, dec_str: &str| -> anyhow::Result<(f64, f64)> {
                let ra = 2.0 * PI * ra_str.parse::<f64>()? / 24.0;
                let dec = 2.0 * PI * dec_str.parse::<f64>()? / 360.0;
                Ok((ra, dec))
            };
            match line_items.as_slice() {
                &[ra_str, dec_str, "start", item_name] => {
                    name = item_name.to_string();
                    points.clear();
                    let (ra, dec) = parse_coords(ra_str, dec_str)?;
                    points.push(ObjEqCoord::new(ra, dec));
                },
                &[ra_str, dec_str, "end"] => {
                    let (ra, dec) = parse_coords(ra_str, dec_str)?;
                    points.push(ObjEqCoord::new(ra, dec));
                    self.outlines.push(Outline{
                        name: std::mem::take(&mut name),
                        polygon: std::mem::take(&mut points),
                    });
                }
                &[ra_str, dec_str, "vertex"] => {
                    let (ra, dec) = parse_coords(ra_str, dec_str)?;
                    points.push(ObjEqCoord::new(ra, dec));
                },
                _ => {
                    println!("Strange outline record: {}", line);
                },
            }
        }
        Ok(())
    }

    pub fn get_nearest(
        &self,
        crd:          &EqCoord,
        max_dso_mag:  f32,
        max_star_mag: f32
    ) -> Option<Object> {
        let nearest_star = self.stars.get_nearest(crd, max_star_mag);
        let nearest_obj = self.get_nearest_dso_object(crd, max_dso_mag);
        match (nearest_star, nearest_obj) {
            (Some((star, star_angle)), Some((obj, obj_angle))) => {
                if star_angle < obj_angle {
                    Some(Object::Star(star))
                } else {
                    Some(Object::Dso(obj))
                }
            }
            (Some((star, _)), None) =>
                Some(Object::Star(star)),
            (None, Some((obj, _))) =>
                Some(Object::Dso(obj)),
            _ =>
                None
        }
    }

    pub fn get_nearest_dso_object(&self, crd: &EqCoord, max_dso_mag: f32) -> Option<(DsoItem, f64)> {
        let max_mag = ObjMagnitude::new(max_dso_mag);
        let nearest_obj = self.objects.iter()
            .filter(|obj| obj.mag <= max_mag)
            .map(|obj| (obj, EqCoord::angle_between(&obj.crd.to_eq(), crd)))
            .min_by(|(_, angle1), (_, angle2)| f64::total_cmp(&angle1, &angle2));
        nearest_obj.map(|(obj, angle)| (obj.clone(), angle))
    }

}
