//得到概率层次的随机发生器
use rand::distr::Uniform;
use rand::prelude::*;
pub(crate) struct LayerGenerator {
    unif: Uniform<f64>,
    pub scale: f64,
    max_level: usize,
}

impl LayerGenerator {
    pub fn new(max_nb_connection: usize, max_level: usize) -> Self {
        let scale = 1. / (max_nb_connection as f64).ln();
        Self { unif: Uniform::<f64>::new(0., 1.).unwrap(), scale, max_level }
    }

    pub fn generate(&mut self) -> usize {
        let mut rng = rand::rng();
        let level = -rng.sample(self.unif).ln() * self.scale;
        let mut ulevel = level.floor() as usize;
        if ulevel >= self.max_level {
            ulevel = rng.sample(Uniform::<usize>::new(0, self.max_level).unwrap());
        }
        ulevel
    }
}
