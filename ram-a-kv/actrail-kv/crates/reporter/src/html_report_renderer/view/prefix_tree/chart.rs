//! 本文件把前缀链的支持度转换为成员衰减 SVG 所需的坐标模型。

use actrail_kv_artifacts::{PrefixNode, PrefixNodeKind};

pub(crate) struct DecayView {
    pub(crate) grid_lines: Vec<GridLine>,
    pub(crate) points: Vec<ChartPoint>,
    pub(crate) polyline: String,
    pub(crate) ticks: Vec<ChartTick>,
}

pub(crate) struct GridLine {
    pub(crate) y: i32,
    pub(crate) label_y: i32,
    pub(crate) label: &'static str,
}

pub(crate) struct ChartPoint {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) color: &'static str,
}

pub(crate) struct ChartTick {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) index: usize,
}

pub(super) fn build_decay(chain: &[&PrefixNode], member_count: usize) -> DecayView {
    let grid_lines = [(1.0_f64, "100%"), (0.5, "50%"), (0.0, "0%")]
        .into_iter()
        .map(|(ratio, label)| {
            let y = 150 - (ratio * 120.0).round() as i32;
            GridLine {
                y,
                label_y: y + 3,
                label,
            }
        })
        .collect();
    let n = chain.len();
    let mut points = Vec::new();
    for (index, node) in chain.iter().enumerate() {
        let x = if n > 1 {
            24 + index * 592 / (n - 1)
        } else {
            24
        };
        let ratio = node.support_count as f64 / member_count.max(1) as f64;
        let y = 150 - (ratio * 120.0).round() as i32;
        points.push(ChartPoint {
            x: x as i32,
            y,
            color: if node.kind == PrefixNodeKind::Dynamic {
                "#e67e22"
            } else {
                "#2980b9"
            },
        });
    }
    let polyline = points
        .iter()
        .map(|point| format!("{},{} ", point.x, point.y))
        .collect::<String>();
    let label_step = (n / 10).max(1);
    let ticks = chain
        .iter()
        .enumerate()
        .filter(|(index, _)| index % label_step == 0 || *index == n - 1)
        .map(|(index, node)| {
            let x = if n > 1 {
                24 + index * 592 / (n - 1)
            } else {
                24
            };
            let ratio = node.support_count as f64 / member_count.max(1) as f64;
            ChartTick {
                x: x as i32,
                y: 150 - (ratio * 120.0).round() as i32,
                index,
            }
        })
        .collect();
    DecayView {
        grid_lines,
        points,
        polyline,
        ticks,
    }
}
