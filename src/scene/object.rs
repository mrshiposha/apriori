use crate::{
    r#type::{
        ObjectName,
        Mass,
        Distance,
        Color, 
    },
    scene::track::Track,
};

pub struct Object4d {
    track: Track,
    name: ObjectName,
    mass: Mass,
    radius: Distance,
    color: Color,
    is_currently_computing: bool,
}

impl Object4d {
    pub fn new(
        track_size: usize,
        compute_step: chrono::Duration,
        name: ObjectName,
        mass: Mass,
        radius: Distance,
        color: Color,
    ) -> Self {
        Self {
            track: Track::new(track_size, compute_step),
            name,
            mass,
            radius,
            color,
            is_currently_computing: false,
        }
    }

    pub fn name(&self) -> &ObjectName {
        &self.name
    }

    pub fn mass(&self) -> Mass {
        self.mass
    }

    pub fn radius(&self) -> Distance {
        self.radius
    }

    pub fn color(&self) -> &Color {
        &self.color
    }

    pub fn track(&self) -> &Track {
        &self.track
    }

    pub fn track_mut(&mut self) -> &mut Track {
        &mut self.track
    }
}
