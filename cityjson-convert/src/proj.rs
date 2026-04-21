use libc::c_int;
use libc::{c_char, c_double};
use num_traits::Float;
use proj_sys::{
    proj_area_create, proj_area_set_bbox, proj_context_create, proj_context_errno,
    proj_create_crs_to_crs, proj_destroy, proj_errno, proj_errno_reset,
    proj_normalize_for_visualization, proj_trans, PJconsts, PJ_AREA, PJ_CONTEXT, PJ_COORD,
    PJ_DIRECTION_PJ_FWD, PJ_XYZT,
};
use std::ffi::{CStr, CString, NulError};
use std::{fmt::Debug, str};
use thiserror::Error;

pub trait CoordinateType: Float + Copy + PartialOrd + Debug {}

impl<T: Float + Copy + PartialOrd + Debug> CoordinateType for T {}

#[derive(Copy, Clone, Debug)]
pub struct Area {
    pub north: f64,
    pub south: f64,
    pub east: f64,
    pub west: f64,
}

pub trait Coord<T>
where
    T: CoordinateType,
{
    fn x(&self) -> T;
    fn y(&self) -> T;
    fn z(&self) -> T;
    fn from_xyz(x: T, y: T, z: T) -> Self;
}

impl<T: CoordinateType> Coord<T> for (T, T, T) {
    fn x(&self) -> T {
        self.0
    }

    fn y(&self) -> T {
        self.1
    }

    fn z(&self) -> T {
        self.2
    }

    fn from_xyz(x: T, y: T, z: T) -> Self {
        (x, y, z)
    }
}

pub struct Proj {
    pj: *mut PJconsts,
    ctx: *mut PJ_CONTEXT,
    area: Option<*mut PJ_AREA>,
}

impl Proj {
    pub fn new_known_crs(
        from: &str,
        to: &str,
        area: Option<Area>,
    ) -> Result<Self, ProjCreateError> {
        let ctx = unsafe { proj_context_create() };
        transform_epsg(ctx, from, to, area)
    }

    #[allow(clippy::needless_pass_by_value)]
    pub fn convert<C, F>(&self, point: C) -> Result<C, ProjError>
    where
        C: Coord<F>,
        F: CoordinateType,
    {
        let c_x: c_double = point.x().to_f64().ok_or(ProjError::FloatConversion)?;
        let c_y: c_double = point.y().to_f64().ok_or(ProjError::FloatConversion)?;
        let c_z: c_double = point.z().to_f64().ok_or(ProjError::FloatConversion)?;

        let xyzt = PJ_XYZT {
            x: c_x,
            y: c_y,
            z: c_z,
            t: f64::INFINITY,
        };
        let (new_x, new_y, new_z, err) = unsafe {
            proj_errno_reset(self.pj);
            let trans = proj_trans(self.pj, PJ_DIRECTION_PJ_FWD, PJ_COORD { xyzt });
            (trans.xyz.x, trans.xyz.y, trans.xyz.z, proj_errno(self.pj))
        };

        if err == 0 {
            Ok(C::from_xyz(
                F::from(new_x).ok_or(ProjError::FloatConversion)?,
                F::from(new_y).ok_or(ProjError::FloatConversion)?,
                F::from(new_z).ok_or(ProjError::FloatConversion)?,
            ))
        } else {
            Err(ProjError::Conversion(error_message(err)?))
        }
    }
}

impl Drop for Proj {
    fn drop(&mut self) {
        unsafe {
            proj_destroy(self.pj);
            if let Some(area) = self.area {
                proj_sys::proj_area_destroy(area);
            }
            proj_sys::proj_context_destroy(self.ctx);
        }
    }
}

#[derive(Error, Debug)]
pub enum ProjError {
    #[error("The conversion failed with the following error: {0}")]
    Conversion(String),
    #[error("Couldn't create a raw pointer from the string")]
    Creation(#[from] NulError),
    #[error("Couldn't convert bytes from PROJ to UTF-8")]
    Utf8Error(#[from] str::Utf8Error),
    #[error("Couldn't convert number to f64")]
    FloatConversion,
}

#[derive(Error, Debug)]
pub enum ProjCreateError {
    #[error("A nul byte was found in the PROJ string definition or CRS argument: {0}")]
    ArgumentNulError(NulError),
    #[error("The underlying PROJ call failed: {0}")]
    ProjError(String),
}

struct Errno(c_int);

impl Errno {
    fn message(&self, context: *mut PJ_CONTEXT) -> String {
        let ptr = unsafe { proj_sys::proj_context_errno_string(context, self.0) };
        if ptr.is_null() {
            panic!("PROJ did not supply an error");
        } else {
            unsafe { raw_string(ptr).expect("PROJ provided an invalid error string") }
        }
    }
}

fn transform_epsg(
    ctx: *mut PJ_CONTEXT,
    from: &str,
    to: &str,
    area: Option<Area>,
) -> Result<Proj, ProjCreateError> {
    let from_c = CString::new(from).map_err(ProjCreateError::ArgumentNulError)?;
    let to_c = CString::new(to).map_err(ProjCreateError::ArgumentNulError)?;
    let proj_area = unsafe { proj_area_create() };
    area_set_bbox(proj_area, area);
    let ptr = result_from_create(ctx, unsafe {
        proj_create_crs_to_crs(ctx, from_c.as_ptr(), to_c.as_ptr(), proj_area)
    })
    .map_err(|e| ProjCreateError::ProjError(e.message(ctx)))?;
    let normalised = unsafe {
        let normalised = proj_normalize_for_visualization(ctx, ptr);
        proj_destroy(ptr);
        normalised
    };
    Ok(Proj {
        pj: normalised,
        ctx,
        area: Some(proj_area),
    })
}

fn area_set_bbox(parea: *mut PJ_AREA, new_area: Option<Area>) {
    if let Some(area) = new_area {
        unsafe {
            proj_area_set_bbox(parea, area.west, area.south, area.east, area.north);
        }
    }
}

fn result_from_create<T>(context: *mut PJ_CONTEXT, ptr: *mut T) -> Result<*mut T, Errno> {
    if ptr.is_null() {
        Err(Errno(unsafe { proj_context_errno(context) }))
    } else {
        Ok(ptr)
    }
}

fn error_message(error: c_int) -> Result<String, str::Utf8Error> {
    let ptr = unsafe { proj_sys::proj_errno_string(error) };
    unsafe { raw_string(ptr) }
}

unsafe fn raw_string(raw_ptr: *const c_char) -> Result<String, str::Utf8Error> {
    assert!(!raw_ptr.is_null());
    let c_str = CStr::from_ptr(raw_ptr);
    Ok(str::from_utf8(c_str.to_bytes())?.to_string())
}
