//! Тесты пространственной сетки: поиск ближайшего, радиус, детерминизм
//! при равных дистанциях и удаление элемента из выдачи.

use crate::simulation::spatial::SpatialGrid;
use crate::simulation::WorldPos;

fn pos(x: f32, y: f32) -> WorldPos {
    WorldPos { x, y }
}

#[test]
fn nearest_respects_radius_and_distance() {
    let mut grid = SpatialGrid::new(8.0);
    grid.insert(1, pos(10.0, 10.0));
    grid.insert(2, pos(12.0, 10.0));
    grid.insert(3, pos(100.0, 100.0));

    assert_eq!(grid.nearest(pos(11.0, 10.0), 5.0).map(|(id, _)| id), Some(1));
    assert_eq!(grid.nearest(pos(12.4, 10.0), 5.0).map(|(id, _)| id), Some(2));
    assert_eq!(grid.nearest(pos(50.0, 50.0), 5.0), None);
}

#[test]
fn nearest_ties_choose_lower_id() {
    let mut grid = SpatialGrid::new(4.0);
    grid.insert(7, pos(10.0, 10.0));
    grid.insert(3, pos(14.0, 10.0));
    // Оба на расстоянии 2 от точки 12:10 — побеждает меньший id.
    assert_eq!(grid.nearest(pos(12.0, 10.0), 3.0).map(|(id, _)| id), Some(3));
}

#[test]
fn remove_hides_item_without_rebuilding() {
    let mut grid = SpatialGrid::new(4.0);
    grid.insert(1, pos(10.0, 10.0));
    grid.insert(2, pos(10.5, 10.0));
    grid.remove(1);
    assert_eq!(grid.nearest(pos(10.0, 10.0), 1.0).map(|(id, _)| id), Some(2));
    grid.clear();
    assert_eq!(grid.nearest(pos(10.0, 10.0), 10.0), None);
}
