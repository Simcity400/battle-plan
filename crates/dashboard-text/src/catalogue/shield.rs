use super::*;
use content_reader_gui::gui::native::{resources::SHIELD_OVERLAY_EFFECT, sprites::SpriteKind};
use content_reader_image::dds;
use shader_pipeline::artwork::Draw;

#[derive(Clone)]
pub struct MaskedShield {
    name: String,
    small: bool,
    effect: Vec<u8>,
    mask: dds::Texture,
    overlay: Option<dds::Texture>,
}

const FLAG_CELL: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

impl MaskedShield {
    pub fn load(
        layers: &ResolvedLayers,
        sprite: &content_reader_gfx::gfx::SpriteType,
        decode: Decode,
    ) -> io::Result<Self> {
        let name = &sprite.name;
        let SpriteKind::MaskedShield { shield, .. } = &sprite.facts.kind else {
            return Err(invalid(format!("{name} is not a maskedShieldType")));
        };
        for (field, value) in [
            ("effectFile", &shield.effect_file),
            ("textureFile2 (its mask)", &shield.texture2),
        ] {
            if value.is_empty() {
                return Err(invalid(format!("{name} declares no {field}")));
            }
        }
        let named = |why: io::Error| invalid(format!("{name}: {why}"));
        let overlay = if shield.texture1.is_empty() {
            None
        } else {
            sprite::optional(layers, &shield.texture1, decode).map_err(named)?
        };
        Ok(Self {
            name: name.clone(),
            small: shield.small,
            effect: shield.effect_file.clone(),
            mask: sprite::load(layers, &shield.texture2, decode).map_err(named)?,
            overlay,
        })
    }

    pub fn small(&self) -> bool {
        self.small
    }

    pub fn overlay(&self) -> io::Result<Option<Rgba8Image>> {
        self.overlay
            .clone()
            .map(content_reader_image::rgba::dds_base)
            .transpose()
    }

    pub fn with_overlay(&self, overlay: Rgba8Image) -> Self {
        Self {
            overlay: Some(dds::Texture::from_rgba8(overlay)),
            ..self.clone()
        }
    }

    pub fn prepare(&self, layers: &ResolvedLayers, decode: Decode) {
        let mut effects = vec![self.effect.as_slice()];
        if self.overlay.is_some() {
            effects.push(SHIELD_OVERLAY_EFFECT.as_bytes());
        }
        for effect in effects {
            let _ = shader_pipeline::artwork::prepare_sprite(layers, effect, decode);
        }
    }

    pub fn draw(
        &self,
        layers: &ResolvedLayers,
        flags: &[&Rgba8Image],
        decode: Decode,
    ) -> io::Result<Vec<Rgba8Image>> {
        let refuse = |why: String| invalid(format!("shield {}: {why}", self.name));
        shader_pipeline::artwork::flag_rectangle_mapped(layers, &self.effect, decode)
            .map_err(refuse)?;
        let drawables = flags
            .iter()
            .map(|flag| self.passes(&dds::Texture::from_rgba8((*flag).clone())))
            .collect();
        shader_pipeline::artwork::render_layers(layers, drawables, decode).map_err(refuse)
    }

    #[expect(clippy::single_range_in_vec_init, reason = "a draw lists the byte ranges of its constants, and each of these uploads one")]
    fn passes(&self, flag: &dds::Texture) -> Vec<Draw> {
        let overlay = self.overlay.as_ref().map_or([0.0; 2], |overlay| {
            [overlay.width, overlay.height].map(|edge| edge as f32)
        });
        let mask = [self.mask.width, self.mask.height].map(|edge| edge as f32);
        let half = [0, 1].map(|axis| (overlay[axis] - mask[axis]) * 0.5);
        let overlay_rect = [0.0, -1.0, overlay[0], overlay[1]];
        let flag_rect = [half[0], half[1] - 1.0, mask[0], mask[1]];
        let low = [0, 1].map(|axis| {
            let rects = [overlay_rect, flag_rect];
            rects
                .iter()
                .map(|rect| rect[axis])
                .fold(f32::INFINITY, f32::min)
                .floor()
        });
        let high = [0, 1].map(|axis| {
            [overlay_rect, flag_rect]
                .iter()
                .map(|rect| rect[axis] + rect[axis + 2])
                .fold(f32::NEG_INFINITY, f32::max)
                .ceil()
        });
        let size = [0, 1].map(|axis| (high[axis] - low[axis]) as u32);
        let matrix = projection(size, low);
        let mut flag_constants = matrix.to_vec();
        flag_constants.extend(FLAG_CELL);
        flag_constants.extend([0.0; 4]);
        let mut passes = vec![Draw {
            effect: self.effect.clone(),
            defines: Vec::new(),
            size,
            vertices: quad(flag_rect),
            constants: vec![(0, flag_constants, vec![0..96])],
            pixel_constants: false,
            textures: vec![(0, Some(flag.clone())), (1, Some(self.mask.clone()))],
        }];
        if let Some(texture) = &self.overlay {
            let mut overlay_constants = matrix.to_vec();
            overlay_constants.extend([1.0; 4]);
            overlay_constants.resize(28, 0.0);
            passes.push(Draw {
                effect: SHIELD_OVERLAY_EFFECT.as_bytes().to_vec(),
                defines: Vec::new(),
                size,
                vertices: quad(overlay_rect),
                constants: vec![(0, overlay_constants, vec![0..104])],
                pixel_constants: true,
                textures: vec![(0, Some(texture.clone())), (1, Some(self.mask.clone()))],
            });
        }
        passes
    }
}

fn projection(size: [u32; 2], low: [f32; 2]) -> [f32; 16] {
    let [w, h] = size.map(|edge| edge as f32);
    [
        2.0 / w,
        0.0,
        0.0,
        0.0,
        0.0,
        2.0 / h,
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
        0.0,
        -1.0 - 2.0 * low[0] / w,
        -1.0 - 2.0 * low[1] / h,
        -1.0,
        1.0,
    ]
}

fn quad([x, y, w, h]: [f32; 4]) -> [[f32; 5]; 4] {
    [
        [x, y, 1.0, 0.0, 1.0],
        [x, y + h, 1.0, 0.0, 0.0],
        [x + w, y, 1.0, 1.0, 1.0],
        [x + w, y + h, 1.0, 1.0, 0.0],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use dashboard_core::test_fixtures::game;

    fn shield(overlay: Option<[u32; 2]>, mask: [u32; 2]) -> MaskedShield {
        let texture = |[width, height]: [u32; 2]| {
            dds::Texture::from_rgba8(Rgba8Image {
                width,
                height,
                pixels: vec![255; (width * height * 4) as usize],
            })
        };
        MaskedShield {
            name: "GFX_fixture_shield".into(),
            small: false,
            effect: b"gfx/FX/fixture_flag.lua".to_vec(),
            mask: texture(mask),
            overlay: overlay.map(texture),
        }
    }

    fn flag() -> dds::Texture {
        dds::Texture::from_rgba8(Rgba8Image {
            width: 2,
            height: 1,
            pixels: vec![9; 8],
        })
    }

    #[test]
    #[ignore = "requires DXC and a GPU; synthetic content only"]
    fn shields_composite_the_flag_and_overlay_passes_with_their_authored_blend() {
        use super::super::texture_tests::{
            from_url, png_bytes, shield_flag_program, shield_programs,
        };
        game::enter();
        let gfx = "spriteTypes = {
            maskedShieldType = { name = GFX_fixture_shield textureFile1 = gfx/overlay.png textureFile2 = gfx/mask.png effectFile = gfx/FX/shield_flag.lua }
            maskedShieldType = { name = GFX_additive_shield textureFile1 = gfx/overlay.png textureFile2 = gfx/mask.png effectFile = gfx/FX/additive_flag.lua }
            maskedShieldType = { name = GFX_bare_shield textureFile2 = gfx/mask.png effectFile = gfx/FX/shield_flag.lua }
            maskedShieldType = { name = GFX_atlas_shield textureFile1 = gfx/overlay.png textureFile2 = gfx/mask.png effectFile = gfx/FX/atlas_flag.lua }
        }";
        let mut programs = shield_programs();
        programs.push((
            "gfx/FX/additive_flag.shader",
            shield_flag_program("SourceBlend = \"ONE\" DestBlend = \"ONE\""),
        ));
        let atlas =
            shield_flag_program("SourceBlend = \"SRC_ALPHA\" DestBlend = \"INV_SRC_ALPHA\"")
                .replace(
                    "FlagCoords.xy + i.uv * FlagCoords.zw",
                    "FlagCoords.zw * 0.75",
                );
        programs.push(("gfx/FX/atlas_flag.shader", atlas));
        let mut files: Vec<_> = programs
            .iter()
            .map(|(path, text)| (*path, text.as_str()))
            .collect();
        files.push(("interface/shield.gfx", gfx));
        let (catalogue, folder) = super::super::tests::folder_catalogue(&files);
        let image = |width, height, pixel: &dyn Fn(u32) -> [u8; 4]| Rgba8Image {
            width,
            height,
            pixels: (0..width * height)
                .flat_map(|at| pixel(at % width))
                .collect(),
        };
        let mask = image(4, 2, &|x| [255, 255, 255, if x < 2 { 255 } else { 0 }]);
        for (path, texture) in [
            ("gfx/mask.png", mask),
            ("gfx/overlay.png", image(6, 4, &|_| [0, 0, 0, 128])),
        ] {
            std::fs::write(folder.0.join(path), png_bytes(&texture)).unwrap();
        }
        let layers = &catalogue.content.layers;
        let load = |name| {
            MaskedShield::load(
                layers,
                catalogue.sprites.lookup(name).unwrap(),
                Decode::default(),
            )
            .unwrap()
        };
        let flag = image(2, 1, &|x| {
            if x == 0 {
                [200, 0, 0, 90]
            } else {
                [0, 0, 200, 90]
            }
        });
        let drawn = load("GFX_fixture_shield")
            .draw(layers, &[&flag, &flag], Decode::default())
            .unwrap();
        assert_eq!(drawn.len(), 2);
        let drawn = from_url(&png_url(&drawn[0]).unwrap());
        assert_eq!([drawn.width, drawn.height], [6, 4]);
        for y in 0..4 {
            for x in 0..6 {
                let at = ((y * 6 + x) * 4) as usize;
                let pixel = &drawn.pixels[at..at + 4];
                let expected = if (1..3).contains(&x) && (1..3).contains(&y) {
                    [100, 0, 0, 255]
                } else {
                    [0, 0, 0, 128]
                };
                assert!(
                    pixel.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 1),
                    "({x}, {y}): {pixel:?}"
                );
            }
        }
        let why = load("GFX_additive_shield")
            .draw(layers, &[&flag], Decode::default())
            .unwrap_err()
            .to_string();
        assert!(
            why.contains("GFX_additive_shield")
                && why.contains("additive_flag")
                && why.contains("only a source-over pass"),
            "{why}"
        );
        let bare = load("GFX_bare_shield")
            .draw(layers, &[&flag], Decode::default())
            .unwrap();
        let bare = from_url(&png_url(&bare[0]).unwrap());
        assert_eq!([bare.width, bare.height], [4, 2]);
        assert_eq!(&bare.pixels[..4], [200, 0, 0, 255]);
        assert_eq!(bare.pixels[3 * 4 + 3], 0);
        let why = load("GFX_atlas_shield")
            .draw(layers, &[&flag], Decode::default())
            .unwrap_err()
            .to_string();
        assert!(
            why.contains("GFX_atlas_shield")
                && why.contains("atlas_flag")
                && why.contains("rectangle"),
            "{why}"
        );
    }

    #[test]
    fn shields_refuse_other_kinds_and_missing_fields_by_name_and_draw_without_an_overlay() {
        game::enter();
        let (catalogue, folder) = super::super::tests::folder_catalogue(&[(
            "interface/shield.gfx",
            "spriteTypes = {
                spriteType = { name = GFX_plain textureFile = gfx/a.png }
                maskedShieldType = { name = GFX_no_effect textureFile1 = gfx/a.png textureFile2 = gfx/a.png }
                maskedShieldType = { name = GFX_no_mask textureFile1 = gfx/a.png effectFile = gfx/FX/a.lua }
                maskedShieldType = { name = GFX_no_overlay textureFile2 = gfx/a.png effectFile = gfx/FX/a.lua }
                maskedShieldType = { name = GFX_lost_overlay textureFile1 = gfx/absent.png textureFile2 = gfx/a.png effectFile = gfx/FX/a.lua }
                maskedShieldType = { name = GFX_absent textureFile1 = gfx/absent.png textureFile2 = gfx/absent.png effectFile = gfx/FX/a.lua }
            }",
        )]);
        for (name, expected) in [
            ("GFX_plain", "GFX_plain is not a maskedShieldType"),
            ("GFX_no_effect", "GFX_no_effect declares no effectFile"),
            ("GFX_no_mask", "GFX_no_mask declares no textureFile2"),
            (
                "GFX_absent",
                "GFX_absent: GUI texture gfx/absent.png is unavailable",
            ),
        ] {
            let why = MaskedShield::load(
                &catalogue.content.layers,
                catalogue.sprites.lookup(name).unwrap(),
                Decode::default(),
            )
            .err()
            .unwrap()
            .to_string();
            assert!(why.contains(expected), "{name}: {why}");
        }
        let mask = Rgba8Image {
            width: 2,
            height: 2,
            pixels: vec![255; 16],
        };
        std::fs::write(
            folder.0.join("gfx/a.png"),
            super::super::texture_tests::png_bytes(&mask),
        )
        .unwrap();
        for name in ["GFX_no_overlay", "GFX_lost_overlay"] {
            let shield = MaskedShield::load(
                &catalogue.content.layers,
                catalogue.sprites.lookup(name).unwrap(),
                Decode::default(),
            )
            .unwrap();
            assert!(shield.overlay().unwrap().is_none(), "{name}");
        }
    }
}
