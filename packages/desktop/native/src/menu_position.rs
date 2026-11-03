// Mirrors components/ui/anchor.ts; all rectangles use the same display coordinate space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Placement {
    Top,
    Bottom,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Alignment {
    Start,
    Center,
    End,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Input {
    pub trigger_rect: Rect,
    pub content_size: Size,
    pub display_area: Rect,
    pub placement: Placement,
    pub alignment: Alignment,
    pub offset: f64,
    pub flip_horizontal: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Position {
    pub x: f64,
    pub y: f64,
    pub actual_placement: Placement,
}

// Authored EDGE_PADDING in components/ui/anchor.ts.
pub const EDGE_PADDING: f64 = 8.;

pub fn compute_position(input: Input) -> Position {
    let Input {
        trigger_rect: trigger,
        content_size: content,
        display_area: display,
        placement,
        alignment,
        offset,
        flip_horizontal,
    } = input;
    let space_top = trigger.y - display.y;
    let space_bottom = display.y + display.height - (trigger.y + trigger.height);
    let space_left = trigger.x - display.x;
    let space_right = display.x + display.width - (trigger.x + trigger.width);
    // Flipping compares the unpadded space with content, not content plus the offset.
    let actual_placement = match placement {
        Placement::Bottom if space_bottom < content.height && space_top > space_bottom => {
            Placement::Top
        }
        Placement::Top if space_top < content.height && space_bottom > space_top => {
            Placement::Bottom
        }
        Placement::Right
            if flip_horizontal && space_right < content.width && space_left > space_right =>
        {
            Placement::Left
        }
        Placement::Left
            if flip_horizontal && space_left < content.width && space_right > space_left =>
        {
            Placement::Right
        }
        _ => placement,
    };
    let (x, y) = match actual_placement {
        Placement::Left => (trigger.x - content.width - offset, trigger.y),
        Placement::Right => (trigger.x + trigger.width + offset, trigger.y),
        vertical => {
            let y = match vertical {
                Placement::Bottom => trigger.y + trigger.height + offset,
                _ => trigger.y - content.height - offset,
            };
            let x = match alignment {
                Alignment::Start => trigger.x,
                Alignment::End => trigger.x + trigger.width - content.width,
                Alignment::Center => trigger.x + (trigger.width - content.width) / 2.,
            };
            (x, y)
        }
    };
    // min/max, rather than clamp, keeps oversized content at the leading padded edge.
    Position {
        x: (display.x + EDGE_PADDING)
            .max((display.x + display.width - content.width - EDGE_PADDING).min(x)),
        y: (display.y + EDGE_PADDING)
            .max((display.y + display.height - content.height - EDGE_PADDING).min(y)),
        actual_placement,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> Input {
        Input {
            trigger_rect: Rect {
                x: 100.,
                y: 100.,
                width: 40.,
                height: 20.,
            },
            content_size: Size {
                width: 200.,
                height: 100.,
            },
            display_area: Rect {
                x: 0.,
                y: 0.,
                width: 1000.,
                height: 800.,
            },
            placement: Placement::Bottom,
            alignment: Alignment::Start,
            offset: 4.,
            flip_horizontal: false,
        }
    }

    #[test]
    fn authored_anchor_alignment_cases() {
        for (alignment, x) in [
            (Alignment::Start, 100.),
            (Alignment::Center, 20.),
            (Alignment::End, 8.),
        ] {
            assert_eq!(
                compute_position(Input {
                    alignment,
                    ..input()
                }),
                Position {
                    x,
                    y: 124.,
                    actual_placement: Placement::Bottom
                }
            );
        }
    }

    #[test]
    fn flips_above_the_complete_trigger_instead_of_snapping_over_it() {
        let mut case = input();
        case.trigger_rect.y = 700.;
        case.content_size.height = 200.;
        assert_eq!(
            compute_position(case),
            Position {
                x: 100.,
                y: 496.,
                actual_placement: Placement::Top
            }
        );
    }

    #[test]
    fn flips_to_the_roomier_side_even_when_neither_side_fits() {
        let mut case = input();
        case.trigger_rect.y = 550.;
        case.content_size.height = 600.;
        assert_eq!(
            compute_position(case),
            Position {
                x: 100.,
                y: 8.,
                actual_placement: Placement::Top
            }
        );
    }

    #[test]
    fn stays_when_the_opposite_side_is_no_roomier() {
        let mut case = input();
        case.display_area.height = 300.;
        case.content_size.height = 400.;
        assert_eq!(
            compute_position(case),
            Position {
                x: 100.,
                y: 8.,
                actual_placement: Placement::Bottom
            }
        );
        case.trigger_rect.y = 140.;
        assert_eq!(compute_position(case).actual_placement, Placement::Bottom);
        case.placement = Placement::Top;
        assert_eq!(compute_position(case).actual_placement, Placement::Top);
    }

    #[test]
    fn flips_down_and_keeps_end_alignment() {
        let mut case = input();
        case.trigger_rect.x = 400.;
        case.trigger_rect.y = 50.;
        case.placement = Placement::Top;
        case.alignment = Alignment::End;
        assert_eq!(
            compute_position(case),
            Position {
                x: 240.,
                y: 74.,
                actual_placement: Placement::Bottom
            }
        );
    }

    #[test]
    fn offset_and_edge_padding_do_not_change_the_flip_threshold() {
        let mut case = input();
        case.trigger_rect.y = 680.;
        assert_eq!(
            compute_position(case),
            Position {
                x: 100.,
                y: 692.,
                actual_placement: Placement::Bottom
            }
        );
        case.trigger_rect.y += 1.;
        assert_eq!(
            compute_position(case),
            Position {
                x: 100.,
                y: 577.,
                actual_placement: Placement::Top
            }
        );
    }

    #[test]
    fn side_placements_ignore_alignment_and_flip_only_when_requested() {
        for alignment in [Alignment::Start, Alignment::Center, Alignment::End] {
            assert_eq!(
                compute_position(Input {
                    placement: Placement::Left,
                    alignment,
                    ..input()
                }),
                Position {
                    x: 8.,
                    y: 100.,
                    actual_placement: Placement::Left
                }
            );
            assert_eq!(
                compute_position(Input {
                    placement: Placement::Right,
                    alignment,
                    ..input()
                }),
                Position {
                    x: 144.,
                    y: 100.,
                    actual_placement: Placement::Right
                }
            );
        }
        let mut case = input();
        case.trigger_rect.x = 850.;
        case.placement = Placement::Right;
        assert_eq!(
            compute_position(case),
            Position {
                x: 792.,
                y: 100.,
                actual_placement: Placement::Right
            }
        );
        case.flip_horizontal = true;
        assert_eq!(
            compute_position(case),
            Position {
                x: 646.,
                y: 100.,
                actual_placement: Placement::Left
            }
        );
        case.trigger_rect.x = 50.;
        case.placement = Placement::Left;
        assert_eq!(
            compute_position(case),
            Position {
                x: 94.,
                y: 100.,
                actual_placement: Placement::Right
            }
        );
    }

    #[test]
    fn all_edges_are_padded_even_without_window_overflow() {
        let mut case = input();
        case.trigger_rect.x = 2.;
        assert_eq!(compute_position(case).x, 8.);
        case.trigger_rect.x = 795.;
        assert_eq!(compute_position(case).x, 792.);
        case.trigger_rect.y = -20.;
        assert_eq!(compute_position(case).y, 8.);
    }

    #[test]
    fn offset_display_and_oversized_content_keep_the_leading_padding() {
        let mut case = input();
        case.display_area = Rect {
            x: 100.,
            y: 200.,
            width: 900.,
            height: 600.,
        };
        case.alignment = Alignment::End;
        assert_eq!(
            compute_position(case),
            Position {
                x: 108.,
                y: 208.,
                actual_placement: Placement::Bottom
            }
        );
        case.content_size = Size {
            width: 1200.,
            height: 900.,
        };
        assert_eq!(
            compute_position(case),
            Position {
                x: 108.,
                y: 208.,
                actual_placement: Placement::Bottom
            }
        );
    }

    #[test]
    fn pointer_context_anchor_preserves_gap_and_vertical_flip() {
        let mut case = input();
        case.trigger_rect = Rect {
            x: 300.,
            y: 701.,
            width: 0.,
            height: 0.,
        };
        assert_eq!(
            compute_position(case),
            Position {
                x: 300.,
                y: 597.,
                actual_placement: Placement::Top
            }
        );
        case.trigger_rect.y = 100.;
        assert_eq!(
            compute_position(case),
            Position {
                x: 300.,
                y: 104.,
                actual_placement: Placement::Bottom
            }
        );
    }

    #[test]
    fn fractional_measurements_are_not_rounded() {
        let mut case = input();
        case.trigger_rect.x = 400.25;
        case.trigger_rect.width = 27.5;
        case.content_size.width = 201.25;
        case.alignment = Alignment::Center;
        assert_eq!(compute_position(case).x, 313.375);
    }

    #[test]
    fn resized_display_and_growing_content_recompute_placement() {
        let mut case = input();
        case.trigger_rect.y = 450.;
        assert_eq!(compute_position(case).y, 474.);
        case.content_size.height = 400.;
        assert_eq!(
            compute_position(case),
            Position {
                x: 100.,
                y: 46.,
                actual_placement: Placement::Top
            }
        );
        case.display_area.height = 1200.;
        assert_eq!(
            compute_position(case),
            Position {
                x: 100.,
                y: 474.,
                actual_placement: Placement::Bottom
            }
        );
    }
}
