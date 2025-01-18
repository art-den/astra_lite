#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("IO error: `{0}`")]
    IO(#[from] std::io::Error),

    #[error("Internal error: `{0}`")]
    Internal(String),

    #[error("XML error: `{0}`")]
    Xml(String),

    #[error("Device `{0}` not found")]
    DeviceNotExists(String),

    #[error("Property `{1}` of device `{0}` not found")]
    PropertyNotExists(String, String),

    #[error("No one of properties {0} found of device `{1}`")]
    NoOnePropertyFound(String, String),

    #[error("Property `{1}` of device `{0}` is read only")]
    PropertyIsReadOnly(String, String),

    #[error("Element `{2}` of property `{1}` of device `{0}` not found")]
    PropertyElemNotExists(String, String, String),

    #[error("Property `{1}` of device `{0}` has type {2} but {3} required")]
    WrongPropertyType(String, String, String, String),

    #[error("{0}")]
    WrongArgument(String),

    #[error("Wrong sequense: {0}")]
    WrongSequense(String),

    #[error("Can't convert property value {0} of type {1} into type {2}")]
    CantConvertPropValue(String, String, String),
}

pub type Result<T> = std::result::Result<T, Error>;
