use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderFixture {
    pub name: String,
    pub value: String,
}

impl HeaderFixture {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BodyFixture {
    Empty,
    Json(String),
    Binary(Vec<u8>),
    Text(String),
}

impl BodyFixture {
    pub fn json(body: impl Into<String>) -> Self {
        Self::Json(body.into())
    }

    pub fn text(body: impl Into<String>) -> Self {
        Self::Text(body.into())
    }

    pub fn bytes(body: impl Into<Vec<u8>>) -> Self {
        Self::Binary(body.into())
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpFixture {
    pub method: String,
    pub path: String,
    pub headers: Vec<HeaderFixture>,
    pub body: BodyFixture,
}

impl HttpFixture {
    pub fn new(method: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            path: path.into(),
            headers: Vec::new(),
            body: BodyFixture::Empty,
        }
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push(HeaderFixture::new(name, value));
        self
    }

    pub fn body(mut self, body: BodyFixture) -> Self {
        self.body = body;
        self
    }
}

impl fmt::Display for HttpFixture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.method, self.path)
    }
}
