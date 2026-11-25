use std::ops::{Add, AddAssign, Div, Mul, Sub, SubAssign};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    pub fn is_inside_polygon(&self, vertices: &[Point]) -> bool {
        if vertices.len() < 3 {
            return false;
        }

        // on the same side of every edge, or on one; `signum` would give ±1 for ±0
        let crosses = vertices
            .iter()
            .zip(vertices.iter().cycle().skip(1))
            .map(|(&a, &b)| (b - a).cross(*self - a));
        let (mut left, mut right) = (false, false);
        for cross in crosses {
            left |= cross > 0.0;
            right |= cross < 0.0;
        }
        !(left && right)
    }
}

impl Add<Vector> for Point {
    type Output = Point;

    fn add(self, rhs: Vector) -> Self::Output {
        Point {
            x: self.x + rhs.x,
            y: self.y + rhs.y,
        }
    }
}

impl Sub for Point {
    type Output = Vector;

    fn sub(self, rhs: Point) -> Self::Output {
        Vector {
            x: self.x - rhs.x,
            y: self.y - rhs.y,
        }
    }
}

impl Sub<Vector> for Point {
    type Output = Point;

    fn sub(self, rhs: Vector) -> Self::Output {
        Point {
            x: self.x - rhs.x,
            y: self.y - rhs.y,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vector {
    pub x: f64,
    pub y: f64,
}

impl Vector {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    pub fn length(&self) -> f64 {
        (self.x.powi(2) + self.y.powi(2)).sqrt()
    }

    pub fn zero() -> Self {
        Self { x: 0.0, y: 0.0 }
    }

    pub fn normalize(&self) -> Vector {
        let len = self.length();
        if len == 0.0 {
            Vector::zero()
        } else {
            *self / len
        }
    }

    pub fn perpendicular(&self) -> Vector {
        Vector::new(-self.y, self.x)
    }

    pub fn dot(&self, other: Vector) -> f64 {
        self.x * other.x + self.y * other.y
    }

    pub fn cross(&self, other: Vector) -> f64 {
        self.x * other.y - self.y * other.x
    }
}

impl Add for Vector {
    type Output = Vector;

    fn add(self, rhs: Vector) -> Self::Output {
        Vector {
            x: self.x + rhs.x,
            y: self.y + rhs.y,
        }
    }
}

impl AddAssign for Vector {
    fn add_assign(&mut self, rhs: Self) {
        self.x += rhs.x;
        self.y += rhs.y;
    }
}

impl Sub for Vector {
    type Output = Vector;

    fn sub(self, rhs: Vector) -> Self::Output {
        Vector {
            x: self.x - rhs.x,
            y: self.y - rhs.y,
        }
    }
}

impl SubAssign for Vector {
    fn sub_assign(&mut self, rhs: Self) {
        self.x -= rhs.x;
        self.y -= rhs.y;
    }
}

impl Mul<f64> for Vector {
    type Output = Vector;

    fn mul(self, rhs: f64) -> Self::Output {
        Vector {
            x: self.x * rhs,
            y: self.y * rhs,
        }
    }
}

impl Mul<Vector> for f64 {
    type Output = Vector;

    fn mul(self, rhs: Vector) -> Self::Output {
        rhs * self
    }
}

impl Div<f64> for Vector {
    type Output = Vector;

    fn div(self, rhs: f64) -> Self::Output {
        Vector {
            x: self.x / rhs,
            y: self.y / rhs,
        }
    }
}

pub fn bounding_box_u32(points: &[Point]) -> (u32, u32, u32, u32) {
    let (mut min_x, mut min_y) = (f64::INFINITY, f64::INFINITY);
    let (mut max_x, mut max_y) = (f64::NEG_INFINITY, f64::NEG_INFINITY);

    for point in points {
        if point.x < min_x {
            min_x = point.x;
        }
        if point.y < min_y {
            min_y = point.y;
        }
        if point.x > max_x {
            max_x = point.x;
        }
        if point.y > max_y {
            max_y = point.y;
        }
    }

    (min_x as u32, min_y as u32, max_x as u32, max_y as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A point on an edge is inside, whichever way the polygon winds: `signum` of 0.0 is 1.0, not
    // 0.0, so it depended on the winding; and a point on the line of the first edge, past it, was
    // inside.
    #[test]
    fn test_inside_polygon_on_its_edges() {
        let ccw = [(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)].map(|(x, y)| Point::new(x, y));
        let mut cw = ccw;
        cw.reverse();
        for square in [ccw, cw] {
            assert!(Point::new(2.0, 0.0).is_inside_polygon(&square));
            assert!(Point::new(2.0, 2.0).is_inside_polygon(&square));
            assert!(!Point::new(6.0, 0.0).is_inside_polygon(&square));
            assert!(!Point::new(5.0, 2.0).is_inside_polygon(&square));
        }
    }
}
