use chr_common::{jsonrpc::ErrorObject, methods::mesh, scene_frame as w};
use serde_json::Value;

pub(super) enum Request {
    Start(mesh::StartRotation),
    Stop(mesh::RotationTarget),
    Get(mesh::RotationTarget),
}

impl Request {
    pub fn parse(method: &str, params: Option<Value>) -> Result<Self, ErrorObject> {
        let params = params.ok_or_else(|| ErrorObject::invalid_params("params are required"))?;
        let parsed = match method {
            mesh::START_ROTATION => serde_json::from_value(params).map(Self::Start),
            mesh::STOP_ROTATION => serde_json::from_value(params).map(Self::Stop),
            mesh::GET_ROTATION => serde_json::from_value(params).map(Self::Get),
            _ => return Err(ErrorObject::method_not_found(method)),
        };
        parsed.map_err(|error| ErrorObject::invalid_params(error.to_string()))
    }

    pub fn target(&self) -> mesh::RotationTarget {
        match self {
            Self::Start(request) => mesh::RotationTarget {
                runtime_id: request.runtime_id.clone(),
                scene_id: request.scene_id.clone(),
                instance_id: request.instance_id.clone(),
            },
            Self::Stop(target) | Self::Get(target) => target.clone(),
        }
    }
}

pub(super) struct Rotation {
    pub state: mesh::RotationState,
    pub base: w::Matrix4,
    pub last: w::Instance,
}

impl Rotation {
    pub fn new(target: mesh::RotationTarget, instance: w::Instance, frame: u64) -> Self {
        Self {
            state: mesh::RotationState {
                runtime_id: target.runtime_id,
                scene_id: target.scene_id,
                instance_id: target.instance_id,
                rotating: false,
                angle_rad: 0.,
                angular_velocity_rad_per_s: 0.,
                frame,
            },
            base: instance.model,
            last: instance,
        }
    }

    pub fn matches(&self, target: &mesh::RotationTarget) -> bool {
        self.state.runtime_id == target.runtime_id
            && self.state.scene_id == target.scene_id
            && self.state.instance_id == target.instance_id
    }

    pub fn pose(&self, frame: u64, rate: w::TimeBase) -> Result<(f64, w::Instance), ErrorObject> {
        let step = (self.state.angular_velocity_rad_per_s * f64::from(rate.numerator())
            / f64::from(rate.denominator()))
        .rem_euclid(std::f64::consts::TAU);
        let angle = (self.state.angle_rad
            + (frame.saturating_sub(self.state.frame) as f64 * step)
                .rem_euclid(std::f64::consts::TAU))
        .rem_euclid(std::f64::consts::TAU);
        let (s, c) = angle.sin_cos();
        let mut instance = self.last.clone();
        for row in 0..3 {
            let x = self.base[row][0].get();
            let y = self.base[row][1].get();
            instance.model[row][0] = w::Finite::try_from(x * c + y * s)
                .map_err(|error| ErrorObject::invalid_params(error.to_string()))?;
            instance.model[row][1] = w::Finite::try_from(-x * s + y * c)
                .map_err(|error| ErrorObject::invalid_params(error.to_string()))?;
        }
        Ok((angle, instance))
    }
}
