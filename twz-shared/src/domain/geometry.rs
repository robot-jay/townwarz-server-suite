use super::{EntityId, GeometryDefinition};
use crate::{IVec2, TILE_PX};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WorldRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl GeometryDefinition {
    pub fn collider(self, origin: IVec2) -> WorldRect {
        WorldRect {
            x: origin.x as f32 * TILE_PX + self.collision_px.x as f32,
            y: origin.y as f32 * TILE_PX + self.collision_px.y as f32,
            width: self.collision_px.width as f32,
            height: self.collision_px.height as f32,
        }
    }
}

impl WorldRect {
    /// Eject a circle even when its center is inside the rectangle. Closest-
    /// point-only collision otherwise has a zero-length normal in that case.
    pub fn push_circle(self, x: &mut f32, y: &mut f32, radius: f32) -> bool {
        let cx = x.clamp(self.x, self.x + self.width);
        let cy = y.clamp(self.y, self.y + self.height);
        let dx = *x - cx;
        let dy = *y - cy;
        let d2 = dx * dx + dy * dy;
        if d2 >= radius * radius {
            return false;
        }
        if d2 > 0.000001 {
            let d = d2.sqrt();
            *x += dx / d * (radius - d);
            *y += dy / d * (radius - d);
        } else {
            let edges = [
                (*x - self.x, 0),
                (self.x + self.width - *x, 1),
                (*y - self.y, 2),
                (self.y + self.height - *y, 3),
            ];
            match edges
                .into_iter()
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .unwrap()
                .1
            {
                0 => *x = self.x - radius,
                1 => *x = self.x + self.width + radius,
                2 => *y = self.y - radius,
                _ => *y = self.y + self.height + radius,
            }
        }
        true
    }

    /// Segment/AABB slab test prevents fast bullets tunnelling through bodies.
    pub fn intersects_segment(self, from: (f32, f32), to: (f32, f32)) -> bool {
        let mut enter = 0.0f32;
        let mut exit = 1.0f32;
        for (start, delta, low, high) in [
            (from.0, to.0 - from.0, self.x, self.x + self.width),
            (from.1, to.1 - from.1, self.y, self.y + self.height),
        ] {
            if delta.abs() < 0.000001 {
                if start < low || start > high {
                    return false;
                }
            } else {
                let a = (low - start) / delta;
                let b = (high - start) / delta;
                enter = enter.max(a.min(b));
                exit = exit.min(a.max(b));
                if enter > exit {
                    return false;
                }
            }
        }
        true
    }
}

pub fn hall_entity_id(origin: IVec2) -> EntityId {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        format!("townwarz:hall:{}:{}", origin.x, origin.y).as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BuildingType;

    #[test]
    fn forecourt_is_exactly_one_reserved_walkable_row() {
        let geometry = BuildingType::TownHall.definition().geometry;
        assert_eq!(geometry.footprint_tiles, IVec2 { x: 3, y: 3 });
        let rect = geometry.collider(IVec2 { x: 16, y: 16 });
        assert_eq!(rect.height, 96.0);
        let (mut x, mut y) = (840.0, 888.0);
        assert!(!rect.push_circle(&mut x, &mut y, 16.0));
        y = 810.0;
        assert!(rect.push_circle(&mut x, &mut y, 16.0));
        assert!(!rect.push_circle(&mut x, &mut y, 16.0));
        assert!(rect.intersects_segment((700.0, 800.0), (1000.0, 800.0)));
        assert!(!rect.intersects_segment((700.0, 888.0), (1000.0, 888.0)));
    }
}
