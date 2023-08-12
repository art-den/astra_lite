pub struct ObserverData {
    pub latitude:  f64,
    pub longitude: f64,
    pub rotation:  f64, // ???
}

pub struct EqCoord {
    pub dec: f64,
    pub ra: f64,
}

pub struct HorizCoord {
    pub alt: f64,
    pub az: f64,
}

pub fn eq_to_horiz(observer: &ObserverData, eq: &EqCoord) -> HorizCoord {
    todo!()
}

pub fn horiz_to_eq(observer: &ObserverData, horiz: &HorizCoord) -> EqCoord {
    todo!()
}
