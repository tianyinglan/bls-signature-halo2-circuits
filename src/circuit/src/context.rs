/*
  The implementation is ported from https://github.com/DelphinusLab/halo2ecc-s
*/

use crate::assign::{AssignedValue, Cell, Chip, ValueSchema};
use crate::circuit_utils::{
    base_chip::{BaseChip, FIXED_COLUMNS, MUL_COLUMNS, VAR_COLUMNS},
    range_chip::{RangeChip, COMMON_RANGE_BITS, MAX_CHUNKS},
};
use crate::range_info::RangeInfo;
use halo2_proofs::{
    arithmetic::{BaseExt, CurveAffine, FieldExt},
    circuit::{AssignedCell, Region},
    plonk::Error,
};
use std::{
    cell::RefCell,
    fmt::{Display, Formatter},
};
use std::{
    rc::Rc,
    sync::{Arc, Mutex},
};

#[derive(Debug, Clone)]
pub struct Context<N: FieldExt> {
    pub records: Arc<Mutex<Records<N>>>,
    pub base_offset: usize,
    pub range_offset: usize,
}

impl<N: FieldExt> Display for Context<N> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "(range_offset: {}, base_offset: {})",
            self.range_offset, self.base_offset
        )
    }
}

impl<N: FieldExt> Context<N> {
    pub fn new() -> Self {
        Self {
            records: Arc::new(Mutex::new(Records::default())),
            base_offset: 0,
            range_offset: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct IntegerContext<W: BaseExt, N: FieldExt> {
    pub ctx: Rc<RefCell<Context<N>>>,
    pub info: Arc<RangeInfo<W, N>>,
}

impl<W: BaseExt, N: FieldExt> From<IntegerContext<W, N>> for Context<N> {
    fn from(value: IntegerContext<W, N>) -> Self {
        Rc::try_unwrap(value.ctx).unwrap().into_inner()
    }
}

impl<W: BaseExt, N: FieldExt> IntegerContext<W, N> {
    pub fn new(ctx: Rc<RefCell<Context<N>>>) -> Self {
        const OVERFLOW_BITS: u64 = 6;
        Self::new_with_options(ctx, COMMON_RANGE_BITS, OVERFLOW_BITS)
    }

    pub fn new_with_options(
        ctx: Rc<RefCell<Context<N>>>,
        common_range_bits: u64,
        overflow_bits: u64,
    ) -> Self {
        Self {
            ctx,
            info: Arc::new(RangeInfo::<W, N>::new(common_range_bits, overflow_bits)),
        }
    }
}

pub struct NativeScalarEccContext<C: CurveAffine>(
    pub IntegerContext<<C as CurveAffine>::Base, <C as CurveAffine>::ScalarExt>,
);

impl<C: CurveAffine> From<NativeScalarEccContext<C>> for Context<C::Scalar> {
    fn from(value: NativeScalarEccContext<C>) -> Self {
        value.0.into()
    }
}

pub struct GeneralScalarEccContext<C: CurveAffine, N: FieldExt> {
    pub base_integer_ctx: IntegerContext<<C as CurveAffine>::Base, N>,
    pub scalar_integer_ctx: IntegerContext<<C as CurveAffine>::ScalarExt, N>,
    pub native_ctx: Rc<RefCell<Context<N>>>,
}

impl<C: CurveAffine, N: FieldExt> From<GeneralScalarEccContext<C, N>> for Context<N> {
    fn from(value: GeneralScalarEccContext<C, N>) -> Self {
        drop(value.base_integer_ctx);
        drop(value.scalar_integer_ctx);
        Rc::try_unwrap(value.native_ctx).unwrap().into_inner()
    }
}

impl<C: CurveAffine, N: FieldExt> GeneralScalarEccContext<C, N> {
    pub fn new(ctx: Rc<RefCell<Context<N>>>) -> Self {
        Self {
            base_integer_ctx: IntegerContext::<C::Base, N>::new(ctx.clone()),
            scalar_integer_ctx: IntegerContext::<C::Scalar, N>::new(ctx.clone()),
            native_ctx: ctx,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Records<N: FieldExt> {
    pub base_adv_record: Vec<[(Option<N>, bool); VAR_COLUMNS]>,
    pub base_fix_record: Vec<[Option<N>; FIXED_COLUMNS]>,
    pub base_height: usize,

    pub range_adv_record: Vec<(Option<N>, bool)>,
    pub range_fix_record: Vec<[Option<N>; 2]>,
    pub range_height: usize,

    pub permutations: Vec<(Cell, Cell)>,
}

impl<N: FieldExt> Records<N> {
    fn _assign_to_base_chip(
        &self,
        region: &mut Region<'_, N>,
        base_chip: &BaseChip<N>,
    ) -> Result<Vec<Vec<Option<AssignedCell<N, N>>>>, Error> {
        let mut cells = vec![];

        cells.resize(VAR_COLUMNS, vec![None; self.base_height]);

        for (row, advs) in self.base_adv_record.iter().enumerate() {
            if row >= self.base_height {
                break;
            }

            for (col, adv) in advs.iter().enumerate() {
                if adv.0.is_some() {
                    let cell = region.assign_advice(
                        || "base",
                        base_chip.config.base[col],
                        row,
                        || Ok(adv.0.unwrap()),
                    )?;
                    if adv.1 {
                        cells[col][row] = Some(cell);
                    }
                }
            }
        }

        for (row, fixes) in self.base_fix_record.iter().enumerate() {
            if row >= self.base_height {
                break;
            }

            for (col, fix) in fixes.iter().enumerate() {
                if fix.is_some() {
                    let col = if col < VAR_COLUMNS {
                        base_chip.config.coeff[col]
                    } else if col - VAR_COLUMNS < MUL_COLUMNS {
                        base_chip.config.mul_coeff[col - VAR_COLUMNS]
                    } else if col - VAR_COLUMNS - MUL_COLUMNS == 0 {
                        base_chip.config.next_coeff
                    } else {
                        base_chip.config.constant
                    };

                    region.assign_fixed(|| "fix", col, row, || Ok(fix.unwrap()))?;
                }
            }
        }

        Ok(cells)
    }

    pub fn _assign_to_range_chip(
        &self,
        region: &mut Region<'_, N>,
        range_chip: &RangeChip<N>,
    ) -> Result<Vec<Vec<Option<AssignedCell<N, N>>>>, Error> {
        let mut cells = vec![vec![None; self.range_height]];

        for (row, fix) in self.range_fix_record.iter().enumerate() {
            if row >= self.range_height {
                break;
            }
            if fix[0].is_some() {
                region.assign_fixed(
                    || "range block first",
                    range_chip.config.block_first,
                    row,
                    || Ok(fix[0].unwrap()),
                )?;
            }

            if fix[1].is_some() {
                region.assign_fixed(
                    || "range class",
                    range_chip.config.range_class,
                    row,
                    || Ok(fix[1].unwrap()),
                )?;
            }
        }

        for (row, adv) in self.range_adv_record.iter().enumerate() {
            if row >= self.range_height {
                break;
            }
            if adv.0.is_some() {
                let cell = region.assign_advice(
                    || "range var",
                    range_chip.config.value,
                    row,
                    || Ok(adv.0.unwrap()),
                )?;
                if adv.1 {
                    cells[0][row] = Some(cell);
                }
            }
        }

        Ok(cells)
    }

    pub fn _assign_permutation(
        &self,
        region: &mut Region<'_, N>,
        cells: &Vec<Vec<Vec<Option<AssignedCell<N, N>>>>>,
    ) -> Result<(), Error> {
        for (left, right) in self.permutations.iter() {
            let left = cells[left.region as usize][left.col][left.row]
                .as_ref()
                .unwrap()
                .cell();
            let right = cells[right.region as usize][right.col][right.row]
                .as_ref()
                .unwrap()
                .cell();
            region.constrain_equal(left, right)?;
        }

        Ok(())
    }

    pub fn assign_all(
        &self,
        region: &mut Region<'_, N>,
        base_chip: &BaseChip<N>,
        range_chip: &RangeChip<N>,
    ) -> Result<Vec<Vec<Vec<Option<AssignedCell<N, N>>>>>, Error> {
        let base_cells = self._assign_to_base_chip(region, base_chip)?;
        let range_cells = self._assign_to_range_chip(region, range_chip)?;
        let cells = vec![base_cells, range_cells];
        self._assign_permutation(region, &cells)?;
        Ok(cells)
    }

    pub fn enable_permute(&mut self, cell: &Cell) {
        match cell.region {
            Chip::BaseChip => self.base_adv_record[cell.row][cell.col].1 = true,
            Chip::RangeChip => self.range_adv_record[cell.row].1 = true,
        }
    }

    pub fn one_line(
        &mut self,
        offset: usize,
        base_coeff_pairs: Vec<(ValueSchema<N>, N)>,
        constant: Option<N>,
        mul_next_coeffs: (Vec<N>, Option<N>),
    ) {
        assert!(base_coeff_pairs.len() <= VAR_COLUMNS);

        const EXTEND_SIZE: usize = 16;

        if offset >= self.base_adv_record.len() {
            let to_len = (offset + EXTEND_SIZE) & !(EXTEND_SIZE - 1);
            self.base_adv_record
                .resize(to_len, [(None, false); VAR_COLUMNS]);
            self.base_fix_record.resize(to_len, [None; FIXED_COLUMNS]);
        }

        if offset >= self.base_height {
            self.base_height = offset + 1;
        }

        for (i, (base, coeff)) in base_coeff_pairs.into_iter().enumerate() {
            match base.cell() {
                Some(cell) => {
                    let idx = Cell::new(Chip::BaseChip, i, offset);

                    self.base_adv_record[offset][i].1 = true;
                    self.enable_permute(&cell);

                    self.permutations.push((cell, idx));
                }
                _ => {}
            }
            self.base_fix_record[offset][i] = Some(coeff);
            self.base_adv_record[offset][i].0 = Some(base.value());
        }

        let (mul_coeffs, next) = mul_next_coeffs;
        for (i, mul_coeff) in mul_coeffs.into_iter().enumerate() {
            self.base_fix_record[offset][VAR_COLUMNS + i] = Some(mul_coeff);
        }

        if next.is_some() {
            self.base_fix_record[offset][VAR_COLUMNS + MUL_COLUMNS] = next;
        }

        if constant.is_some() {
            self.base_fix_record[offset][VAR_COLUMNS + MUL_COLUMNS + 1] = constant;
        }
    }

    pub fn one_line_with_last(
        &mut self,
        offset: usize,
        base_coeff_pairs: Vec<(ValueSchema<N>, N)>,
        tail: (ValueSchema<N>, N),
        constant: Option<N>,
        mul_next_coeffs: (Vec<N>, Option<N>),
    ) {
        assert!(base_coeff_pairs.len() <= VAR_COLUMNS - 1);

        self.one_line(offset, base_coeff_pairs, constant, mul_next_coeffs);

        let (base, coeff) = tail;

        let i = VAR_COLUMNS - 1;
        match base.cell() {
            Some(cell) => {
                let idx = Cell::new(Chip::BaseChip, i, offset);

                self.base_adv_record[offset][i].1 = true;
                self.enable_permute(&cell);

                self.permutations.push((cell, idx));
            }
            _ => {}
        }
        self.base_fix_record[offset][i] = Some(coeff);
        self.base_adv_record[offset][i].0 = Some(base.value());
    }

    fn ensure_range_record_size(&mut self, offset: usize) {
        const EXTEND_SIZE: usize = 1024;

        if offset >= self.range_adv_record.len() {
            let to_len = (offset + EXTEND_SIZE) & !(EXTEND_SIZE - 1);
            self.range_adv_record.resize(to_len, (None, false));
            self.range_fix_record.resize(to_len, [None; 2]);
        }

        if offset >= self.range_height {
            self.range_height = offset + 1;
        }
    }

    pub fn assign_single_range_value(
        &mut self,
        offset: usize,
        v: N,
        leading_bits: u64,
    ) -> AssignedValue<N> {
        self.ensure_range_record_size(offset + 1);

        self.range_fix_record[offset][1] = Some(N::from(leading_bits));
        self.range_adv_record[offset].0 = Some(v);

        AssignedValue::new(Chip::RangeChip, 0, offset, v)
    }

    pub fn assign_range_value(
        &mut self,
        offset: usize,
        (v, chunks): (N, Vec<N>),
        leading_bits: u64,
    ) -> AssignedValue<N> {
        assert!(chunks.len() as u64 <= MAX_CHUNKS);
        self.ensure_range_record_size(offset + 1 + MAX_CHUNKS as usize);

        self.range_fix_record[offset][0] = Some(N::one());
        self.range_adv_record[offset].0 = Some(v);

        // a row placeholder
        self.range_fix_record[offset + MAX_CHUNKS as usize][0] = Some(N::zero());

        for i in 0..chunks.len() - 1 {
            self.range_fix_record[offset + 1 + i][1] = Some(N::from(COMMON_RANGE_BITS as u64));
        }
        self.range_fix_record[offset + chunks.len()][1] = Some(N::from(leading_bits));

        for i in 0..chunks.len() {
            self.range_adv_record[offset + 1 + i].0 = Some(chunks[i]);
        }
        AssignedValue::new(Chip::RangeChip, 0, offset, v)
    }
}
