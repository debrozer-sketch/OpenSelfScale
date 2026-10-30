
pub struct Screen {
    pub stage: u8,
    pub slint: u8,
    pub title: &'static str,
    pub parent: u8,
    pub back: bool,
}

const EXIT: u8 = 0;

pub const SCREENS: &[Screen] = &[
    Screen { stage: 1, slint: 5, title: "ВХОД ПЕРСОНАЛА", parent: EXIT, back: false },
    Screen { stage: 2, slint: 6, title: "МЕНЮ НАСТРОЕК", parent: EXIT, back: false },
    Screen { stage: 3, slint: 4, title: "ВИТРИНА · КОМПОНОВКА", parent: 12, back: true },
    Screen { stage: 4, slint: 7, title: "ДИАГНОСТИКА ВЕСОВ", parent: 13, back: true },
    Screen { stage: 5, slint: 8, title: "СЕТЬ И ВРЕМЯ", parent: 2, back: true },
    Screen { stage: 6, slint: 9, title: "ПЕЧАТЬ", parent: 2, back: true },
    Screen { stage: 7, slint: 10, title: "СМЕНА ПАРОЛЯ", parent: 25, back: false },
    Screen { stage: 8, slint: 10, title: "СМЕНА ПАРОЛЯ", parent: 25, back: false },
    Screen { stage: 9, slint: 10, title: "СМЕНА ПАРОЛЯ", parent: 25, back: false },
    Screen { stage: 12, slint: 13, title: "ВИТРИНА", parent: 2, back: true },
    Screen { stage: 13, slint: 14, title: "ВЕСЫ И ТОРГОВЛЯ", parent: 2, back: true },
    Screen { stage: 15, slint: 16, title: "ИМПОРТ КОНФИГУРАЦИИ", parent: 25, back: true },
    Screen { stage: 16, slint: 17, title: "ОТЧЁТЫ", parent: 25, back: true },
    Screen { stage: 17, slint: 18, title: "ПРИНТЕР · ДИАГНОСТИКА", parent: 6, back: true },
    Screen { stage: 20, slint: 19, title: "НАСТРОЙКА ВЕСОВ", parent: EXIT, back: false },
    Screen { stage: 21, slint: 20, title: "ЭТИКЕТКИ", parent: 6, back: true },
    Screen { stage: 22, slint: 23, title: "ЖУРНАЛ ДЕЙСТВИЙ", parent: 25, back: true },
    Screen { stage: 23, slint: 24, title: "СОСТОЯНИЕ", parent: 2, back: true },
    Screen { stage: 25, slint: 26, title: "ДОСТУП И СЕРВИС", parent: 2, back: true },
    Screen { stage: 30, slint: 5, title: "ВХОД ПЕРСОНАЛА", parent: EXIT, back: false },
    Screen { stage: 31, slint: 5, title: "ВХОД ПЕРСОНАЛА", parent: EXIT, back: false },
    Screen { stage: 32, slint: 5, title: "ВХОД ПЕРСОНАЛА", parent: EXIT, back: false },
    Screen { stage: 33, slint: 5, title: "ВХОД ПЕРСОНАЛА", parent: EXIT, back: false },
    Screen { stage: 34, slint: 5, title: "ВХОД ПЕРСОНАЛА", parent: EXIT, back: false },
    Screen { stage: 35, slint: 5, title: "ВХОД ПЕРСОНАЛА", parent: EXIT, back: false },
];

const FALLBACK_SLINT: u8 = 7;
const FALLBACK_TITLE: &str = "МЕНЮ НАСТРОЕК";

fn screen_of(stage: u8) -> Option<&'static Screen> {
    SCREENS.iter().find(|s| s.stage == stage)
}

pub fn screen_slint(stage: u8) -> u8 {
    screen_of(stage).map(|s| s.slint).unwrap_or(FALLBACK_SLINT)
}

pub fn screen_title(stage: u8) -> &'static str {
    screen_of(stage).map(|s| s.title).unwrap_or(FALLBACK_TITLE)
}

pub fn screen_parent(stage: u8) -> u8 {
    screen_of(stage).map(|s| s.parent).unwrap_or(EXIT)
}

pub fn screen_parent_for(stage: u8, role: u8) -> u8 {
    let parent = screen_parent(stage);
    if role != 1 && parent != EXIT {
        2
    } else {
        parent
    }
}

pub fn screen_has_back(stage: u8) -> bool {
    screen_of(stage).map(|s| s.back).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_back_screen_has_entry() {
        for s in SCREENS {
            if s.back {
                assert!(
                    s.parent != EXIT,
                    "stage {}: back=true при parent=0 — кнопка ведёт в выход, проверь поведение",
                    s.stage
                );
            }
        }
    }

    #[test]
    fn every_screen_parent_exists() {
        let stages: std::collections::HashSet<u8> = SCREENS.iter().map(|s| s.stage).collect();
        for s in SCREENS {
            if s.parent != EXIT {
                assert!(stages.contains(&s.parent), "stage {}: parent {} не в таблице", s.stage, s.parent);
            }
        }
    }

    #[test]
    fn staff_always_returns_to_main_menu() {
        for s in SCREENS {
            if s.parent != EXIT {
                assert_eq!(screen_parent_for(s.stage, 0), 2, "stage {}", s.stage);
                assert_eq!(screen_parent_for(s.stage, 1), s.parent);
            }
        }
    }

    #[test]
    fn admin_tree_is_two_levels_from_main_menu() {
        let parent = |st: u8| screen_parent(st);
        for top in [12u8, 13, 6, 5, 23, 25] {
            assert_eq!(parent(top), 2, "stage {top}");
        }
        for (child, via) in [(3u8, 12u8), (4, 13), (21, 6), (17, 6), (15, 25), (16, 25), (22, 25)] {
            assert_eq!(parent(child), via, "stage {child}");
        }
    }

    #[test]
    fn known_screens_resolve() {
        assert_eq!(screen_slint(3), 4);
        assert_eq!(screen_slint(22), 23);
        assert_eq!(screen_slint(23), 24);
        assert_eq!(screen_slint(25), 26);
        assert_eq!(screen_title(22), "ЖУРНАЛ ДЕЙСТВИЙ");
        assert_eq!(screen_title(3), "ВИТРИНА · КОМПОНОВКА");
        assert_eq!(screen_parent(3), 12);
        assert_eq!(screen_slint(30), 5);
        assert_eq!(screen_slint(99), FALLBACK_SLINT);
        assert_eq!(screen_title(99), FALLBACK_TITLE);
        assert_eq!(screen_parent(99), EXIT);
    }
}
