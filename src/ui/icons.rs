//! UI-иконки. Отдельного загрузчика больше нет: все текстуры грузит
//! единый реестр `rendering::textures::GameTextures`, UI читает исходные
//! хэндлы напрямую. Псевдоним сохраняет привычные имена методов у систем UI
//! (`icons.tile`, `icons.item`, …).

pub(crate) use crate::rendering::GameTextures as UiIcons;
