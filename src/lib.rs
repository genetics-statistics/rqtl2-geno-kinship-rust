// -*- mode: Rust; rust-indent-unit: 2; -*-
/// @brief Tools for working with RQTL2 format.
///
/// From
/// https://kbroman.org/qtl2/assets/vignettes/user_guide.html#Data_file_format:
///
/// The input data file formats for R/qtl cannot handle complex crosses, and so
/// for R/qtl2, we have defined a new format for the data files. We’ll describe
/// it here briefly; for details, see the separate vignette on the input file
/// format. QTL mapping data consists of a set of tables of data: marker
/// genotypes, phenotypes, marker maps, etc. In the new format, these different
/// tables are in separate comma-delimited (CSV) files. In each file, the first
/// column is a set of IDs for the rows, and the first row is a set of IDs for
/// the columns. For example, the phenotype data file will have individual IDs
/// in the first column and phenotype names in the first row.

pub mod reader;

pub mod util {
  use std::collections::HashMap;
  use std::fs::File;
  use std::io::BufRead;
  use std::io::BufReader;
  use std::io::Seek;
  use std::io::SeekFrom;
  use crate::reader::consume_comments2 as consume_comments2;

  /// @brief Batch size (number of lines to read).
  /// @brief R/QTL2 genotype data file parser.
  ///
  /// @note https://kbroman.org/qtl2/assets/vignettes/input_files.html
  pub struct GenoParser {
    file_reader: BufReader<File>,
    comments: Vec<String>,
    /// @note Markers names.
    markers: Vec<String>,
    /// @note Maps snps value to f64 values. E.g. A to 0.5, B to 1.0, etc.
    hab_mapper: HashMap<char, f64>,
    /// @note File cursor position where SNP records start.
    snp_pos_start: u64,
  }

  impl GenoParser {
    /// @brief Reads file at path.
    ///
    /// @param[in] path      path to R/QTl genotype data file.
    /// @param[in] strip_ids determines whether the first column (IDs) should be omitted.
    pub fn new(path: String, hab_mapper: HashMap<char, f64>) -> std::io::Result<Self> {
      let file = File::open(path)?;
      Self::new_with_file(file, hab_mapper)
    }

    pub fn new_with_file(file: File, hab_mapper: HashMap<char, f64>) -> std::io::Result<Self> {
      let mut file_reader = BufReader::new(file);
      let comments = consume_comments2(&mut file_reader)?;
      let markers = Self::consume_markers(&mut file_reader)?;
      Ok(GenoParser {
        snp_pos_start: file_reader.seek(SeekFrom::Current(0))?,
        file_reader: file_reader,
        comments: comments,
        markers: markers,
        hab_mapper: hab_mapper,
      })
    }

    pub fn iter(&mut self) -> std::io::Result<GenoParserIter> {
      self.file_reader.seek(SeekFrom::Start(self.snp_pos_start))?;
      GenoParserIter::new(&mut self.file_reader, &self.hab_mapper)
    }

    /// @brief Get comments from genotype file.
    pub fn get_comments(&self) -> &Vec<String> {
      &self.comments
    }

    /// @brief Returns vector of tuples (id, snps) parsed from file.
    ///
    /// @note Rewinds file cursor to the beginning of SNP lines after finishing
    /// reading.
    pub fn read_all(&mut self) -> std::io::Result<Vec<(String, Vec<f64>)>> {
      let snps_start_pos = self.file_reader.seek(SeekFrom::Current(0))?;
      let res = read_geno(&mut self.file_reader, &self.hab_mapper);
      self.file_reader.seek(SeekFrom::Start(snps_start_pos))?;
      res
    }

    fn parse_into(
      parsed_snp_buf: &mut [f64],
      snp_line: &String,
      hab_mapper: &HashMap<char, f64>,
    ) -> std::io::Result<()> {
      let io_err = |bad_str: String, msg: &str| {
        std::io::Error::new(
          std::io::ErrorKind::InvalidInput,
          format!("This line <{}> is an invalid SNP record: {}", &bad_str, msg),
        )
      };
      let snp = match snp_line.split('\t').skip(1).next() {
        Some(snp_str) => snp_str,
        None => {
          return Err(io_err(
            snp_line.clone(),
            "snp record and row id should be separated with tab.",
          ))
        }
      };
      if parsed_snp_buf.len() != snp.len() {
        return Err(io_err(
          snp_line.clone(),
          &format!(
            "Invalid record: there are {} markers, however {} SNPs were parsed.",
            parsed_snp_buf.len(),
            snp.len()
          ),
        ));
      }
      for (buf_slot, snp_char) in parsed_snp_buf.iter_mut().zip(snp.chars()) {
        *buf_slot = hab_mapper
          .get(&snp_char)
          .ok_or(io_err(
            String::from(snp),
            &format!(
              "failed to convert character <{}> to a float value.",
              snp_char
            ),
          ))?
          .clone();
      }
      Ok(())
    }

    fn fill_buffer(
      fill_buf: &mut Vec<f64>,
      lines_iter: &mut std::io::Lines<BufReader<&mut File>>,
      snp_line_size: usize,
      hab_mapper: HashMap<char, f64>,
    ) -> std::io::Result<usize> {
      let mut parsed_lines_counter: usize = 0;
      for (line_slice, snp_line) in fill_buf.chunks_mut(snp_line_size).zip(lines_iter) {
        Self::parse_into(line_slice, &snp_line?, &hab_mapper)?;
        parsed_lines_counter += 1;
      }
      Ok(parsed_lines_counter)
    }

    /// @brief Calculates kinship matrix for given geno data reading it in
    /// batches. The amount of buffer, so as memory consumption, depends on the
    /// amount of logical cores on the machine and amount of snps.
    ///
    /// @note The purpose of calculation of Kinship matrix in batches is to not
    /// load a complete SNPs dataset in memory.
    ///
    /// Given a genotype data file stripped to just containing SNPs called G, a
    /// Kinship matrix is a matrix product G.T (transposed) * G.
    ///
    /// The matrix times its transpose forms a symmetrical matrix, thus it is
    /// not needed to calculate the full matrix, just a triangular part of it.
    ///
    /// Matrix multiplication of transposed matrix by itself (or vice versa) can
    /// be performed via addition of non intersecting (1 column with 1 row, 2
    /// column with 2 row) products of columns of matrix's transpose by rows of
    /// matrix.
    ///
    /// [[1, 2],          [[1, 4],     [[1],  * [[1],[4]]     [[2],   * [[2],[5]]
    ///  [4, 5]]     *     [2, 5]]  =   [4]]               +   [5]]                =
    ///
    ///  = [[1,  4],        [[4,  10],      [[5,  14],
    ///     [4, 16]]    +    [10, 25]]  =    [14, 41]]
    ///
    /// When reading a row of genotype matrix, the column of transposed matrix
    /// can be obtained by transposing this row. Thus, it is possible to
    /// represent genotype matrix transpose times itself (G.T * G) product, as
    /// many products of it's columns by rows, and column can be obtained once
    /// the row is read.
    ///
    /// This is how Kinship matrix can be calculated: read batch from file, copy
    /// and transpose it, multiply transposed matrix by non transposed, add to
    /// result Kinship matrix. After each batch will be processed and added to
    /// the result Kinship matrix it will contain the full Kinship matrix.
    ///
    /// Actual algorithm does not involve matrix copying and transposing and
    /// instead just manipulates matrix indices calculation to achieve same
    /// result.
    ///
    /// Since processing of one batch does not depend on the others, the process
    /// of Kinship matrix calculation can be parallelized: each logical thread
    /// gets 2 buffer, first one contains read rows, and a second one stores the
    /// result of batch multiplication, it is done to not block a shared Kinship
    /// matrix buffer while the calculation is in process. When the thread is
    /// spawned, it locks the read and result buffer dispatched to him by a main
    /// thread and then starts the multiplication. Once the multiplication is
    /// finished and a result buffer contains the part of the resulting Kinship
    /// matrix, the thread locks shared Kinship matrix and merges the results
    /// simultaneously nullifying result buffer to not interfere with the
    /// results calculated by the next threads obtaining this buffer, then
    /// messaging the main thread that the buffer pair on this index is freed.
    ///
    /// Main thread works in a loop: loads data, parses it into a read buffer,
    /// dispatches read/result buffer pair to the thread. If all threads are
    /// busy performing calculations, it waits until one of them will put a
    /// freed buffer pair index to the concurrent queue.
    pub fn calc_kinship(&mut self, batch_size: usize) -> std::io::Result<Vec<f64>> {
      if batch_size < 1 {
        panic!("Batch size can't be less than 1.");
      }
      let ids_num = self.markers.len();
      // Kinship matrix is square.
      let common_kinship_matrix: Arc<Mutex<Vec<f64>>> =
        Arc::new(Mutex::new(vec![0.0; ids_num * ids_num]));

      // This amount of snps will be parsed and processed on each iteration.
      let buf_size = ids_num * batch_size;
      // For each physical thread a buffer will be created.
      let buf_num = num_cpus::get();
      use std::sync::{Arc, Mutex};

      // The compiler can't prove that the buffer ownership won't intersect
      // (despite it won't intersect), hence the Arc-Mutex is needed.
      let mut read_bufs = Vec::<Arc<Mutex<Vec<f64>>>>::new();
      let mut kinship_bufs = Vec::<Arc<Mutex<Vec<f64>>>>::new();
      for _ in 0..buf_num {
        read_bufs.push(Arc::new(Mutex::new(vec![0.0; buf_size])));
        kinship_bufs.push(Arc::new(Mutex::new(vec![0.0; ids_num * ids_num])));
      }

      use std::sync::mpsc::channel;
      use std::thread;
      let (kinship_processor, buffer_filler) = channel::<usize>();
      // Fill the concurrent queue with the buffers numbers, so the buffer
      // filler can start parsing into them.
      for buf_idx in 0..buf_num {
        kinship_processor.send(buf_idx).unwrap();
      }
      let mut threads = Vec::<thread::JoinHandle<()>>::new();
      let file_reader = self.file_reader.get_mut();
      let mut line_iter = BufReader::new(file_reader).lines();
      let mut total_snps_read: usize = 0;
      loop {
        // Get freed buffer index.
        let freed_buffer_idx = buffer_filler.recv().unwrap();
        let read_buf = read_bufs[freed_buffer_idx].clone();
        let kins_buf = kinship_bufs[freed_buffer_idx].clone();

        let read_line_amount = match Self::fill_buffer(
          &mut *read_buf.lock().unwrap(),
          &mut line_iter,
          self.markers.len(),
          self.hab_mapper.clone(),
        )? {
          0 => break,
          n => {
            total_snps_read += n;
            n
          }
        };
        // Reached EOF (there is not enough records to form a full batch.).
        // Resize buffer to discard data from previous iterations which was not
        // overwritten because there is not enough lines to fill the whole
        // buffer.
        if read_line_amount < batch_size {
          let buf = &mut *read_buf.lock().unwrap();
          buf.resize(read_line_amount * ids_num, 0.0);
        }
        {
          let (read_buf_arc, kins_buf_arc, res_matrix_arc, kinsh_proc_sender, buf_idx) = (
            read_buf.clone(),
            kins_buf.clone(),
            common_kinship_matrix.clone(),
            kinship_processor.clone(),
            freed_buffer_idx.clone(),
          );

          threads.push(std::thread::spawn(move || {
            let mut threads_read_buf = read_buf_arc.lock().unwrap();
            let mut threads_kins_buf = kins_buf_arc.lock().unwrap();
            calc_partial_kinship(&mut threads_read_buf, &mut threads_kins_buf, ids_num);
            let mut res_matrix = res_matrix_arc.lock().unwrap();
            for (buf_elem, common_matrix_elem) in
              threads_kins_buf.iter_mut().zip(res_matrix.iter_mut())
            {
              *common_matrix_elem += *buf_elem;
              *buf_elem = 0.0;
            }
            kinsh_proc_sender.send(buf_idx).unwrap();
          }));
        }
      }

      assert!(
        total_snps_read >= ids_num,
        format!(
          "Amount of SNPS (lines in file - (1+comments_lines_count)) should be \
           greater or equal to amount of ids \
           (amount of markers). SNP number: {}, IDS number: {}",
          total_snps_read, ids_num
        )
      );

      threads.into_iter().for_each(|thread| {
        thread
          .join()
          .expect("The thread creating or execution failed !")
      });

      self.file_reader.seek(SeekFrom::Start(self.snp_pos_start))?;

      let mut res = Arc::try_unwrap(common_kinship_matrix)
        .expect("Arc uwrapping failed. Kinship matrix is not accessible.")
        .into_inner()
        .expect("Mutex uwrapping failed. Kinship matrix is not accessible.");

      // Mirror Kinship matrix, since only the upper part was calculated (the
      // Kinship matrix is symmetrical because it's formed from it's transpose times itself).
      for i in 0..ids_num {
        let row_length = ids_num;
        for j in 0..i + 1 {
          res[j * row_length + i] /= total_snps_read as f64;
          res[i * row_length + j] = res[j * row_length + i];
        }
      }
      Ok(res)
    }

    /// @brief Consumes markers line from BufRead. File cursor is left right
    /// after comments.
    pub fn consume_markers(file_reader: &mut BufReader<File>) -> std::io::Result<Vec<String>> {
      let mut markers = String::new();
      let start_pos = file_reader.seek(SeekFrom::Current(0))?;
      let markers_len = file_reader.read_line(&mut markers)?;
      file_reader.seek(SeekFrom::Start(start_pos + markers_len as u64))?;
      markers.truncate(markers.len() - 1);
      Ok(
        markers
          .split('\t')
          .skip(1)
          .map(|marker| String::from(marker))
          .collect::<Vec<String>>(),
      )
    }
  }

  pub fn calc_partial_kinship(
    snps: &mut Vec<f64>,
    partial_matrix: &mut Vec<f64>,
    ids_num: usize,
  ) -> () {
    let n = ids_num;
    let k = snps.len() / n;
    // Algorithm from BLAS dsyrk:
    // http://www.netlib.org/lapack/explore-html/d1/d54/group__double__blas__level3_gae0ba56279ae3fa27c75fefbc4cc73ddf.html#gae0ba56279ae3fa27c75fefbc4cc73ddf
    //
    // The BLAS Fortran stores array in a column-major format, but the R/qtl2
    // genotype data stored in a row-major format, so this algorithm corresponds
    // to the branch for non transposed, lower triangular part version, however
    // in fact it performs transposed, upper triangular part multiplication
    // (G.T*G).
    //
    // This algorithm branch (Lower, Non transposed) chosen based on CBLAS
    // http://www.netlib.org/blas/blast-forum/cblas.tgz code for dsyrk
    // (cblas_dsyrk.c), which transforms options (Upper, Transposed) to these
    // arguments when called for row-major matrixes.
    //
    // When the matrix stored in row-major way read in column major way,
    // obtained data is a transpose of this matrix:
    // https://en.wikipedia.org/wiki/Row-_and_column-major_order#Transposition
    //
    // Since this is an exact copy of Fortran code, and Fortran utilizes double
    // index (i,j) to operate over single dimension array (which represents 2D
    // array), the code below performs index flattening for column-major
    // storages exactly how Fortran does. Normally, to flatten index in
    // row-major languages we will multiply row index i by row width and add
    // column index j, here, since this is a direct copy of Fortran code which
    // is a colum-major language, we flatten it as column index j *
    // column height + row index i.
    for j in 0..n {
      for l in 0..k {
        for i in j..n {
          partial_matrix[j * ids_num + i] += snps[l * ids_num + j] * snps[l * ids_num + i];
        }
      }
    }
  }

  /// @brief Reads snps from file.
  /// Returns vector of tuples (id, snps) parsed from file.
  pub fn parse_geno(
    file: &mut File,
    hab_mapper: &HashMap<char, f64>,
  ) -> std::io::Result<Vec<(String, Vec<f64>)>> {
    let mut file_reader = BufReader::new(file.try_clone()?);
    consume_comments2(&mut file_reader)?;
    GenoParser::consume_markers(&mut file_reader)?;
    read_geno(&mut file_reader, hab_mapper)
  }

  /// @note Parse line with markers. File cursor is rewinded to the beginning of
  /// the file.
  /// Example: marker	10	12	38	39	42	54
  pub fn parse_markers(file: &mut File) -> std::io::Result<Vec<String>> {
    let mut buf_reader = BufReader::new(file.try_clone()?);
    consume_comments2(&mut buf_reader)?;
    let res = GenoParser::consume_markers(&mut buf_reader);
    file.seek(SeekFrom::Start(0))?;
    res
  }

  /// @brief Parses comments from the beginning of the file. File cursor is
  /// rewinded to the beginning of the file.
  ///
  /// @note Comments lines example:
  /// # These are comments.
  /// # Only at the beginning of the R/QTL2 file geno file.
  pub fn parse_comments(file: &mut File) -> std::io::Result<Vec<String>> {
    let res = consume_comments2(&mut BufReader::new(file.try_clone()?));
    file.seek(SeekFrom::Start(0))?;
    res
  }

  /// @brief Parses snps from BufRead.
  ///
  /// Returns vector of tuples (id, snps) parsed from file.
  pub fn read_geno(
    file_reader: &mut dyn BufRead,
    hab_mapper: &HashMap<char, f64>,
  ) -> std::io::Result<Vec<(String, Vec<f64>)>> {
    let mut contents = Vec::<(String, Vec<f64>)>::new();
    for line in file_reader.lines() {
      let id_snp_tuple: (String, Vec<f64>) = parse_snp_rec(line?, &hab_mapper)?;
      contents.push(id_snp_tuple);
    }
    Ok(contents)
  }

  /// @brief Parses snp geno record into tuple. Consumes line with record.
  /// <rs41245 AABH> to ("rs41245", Vec<f64>(0.0, 0.0, 1.0, 0.5))
  pub fn parse_snp_rec(
    line: String,
    hab_mapper: &HashMap<char, f64>,
  ) -> std::io::Result<(String, Vec<f64>)> {
    let line_str = line;
    let mut id_snp = line_str.split('\t');
    let id = id_snp.next().unwrap();
    let parse_snps = |snp_str: &str| {
      snp_str
        .chars()
        .map(|ch| {
          hab_mapper
            .get(&ch)
            .map(|v| *v)
            .clone()
            .expect(&format!("No key <{}> in SNP mapper.", ch)[..])
        })
        .collect::<Vec<f64>>()
    };
    let id_snp_tuple: (String, Vec<f64>) = (
      String::from(id),
      parse_snps(&id_snp.next().ok_or(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("This line <{}> is an invalid SNP record.", line_str),
      ))?),
    );
    Ok(id_snp_tuple)
  }

  /// @brief Parses lines from genotype file.
  pub struct GenoParserIter<'a> {
    lines_reader: std::io::Lines<&'a mut BufReader<File>>,
    hab_mapper: &'a HashMap<char, f64>,
  }

  impl<'a> GenoParserIter<'a> {
    /// @note File cursor must be located at the beginning of SNP records.
    fn new(
      file_reader: &'a mut BufReader<File>,
      hab_mapper: &'a HashMap<char, f64>,
    ) -> std::io::Result<Self> {
      Ok(Self {
        lines_reader: file_reader.lines(),
        hab_mapper: hab_mapper,
      })
    }
  }

  impl<'a> Iterator for GenoParserIter<'a> {
    type Item = (String, Vec<f64>);

    /// @brief Parse next line from genotype file. Returns tuple (row_id, snps).
    fn next(&mut self) -> Option<Self::Item> {
      // While EOF is not reached (and until buffer is filled).
      match self.lines_reader.next() {
        Some(line) => {
          return match parse_snp_rec(line.expect("Failed to read SNP line."), &self.hab_mapper) {
            Ok(val) => Some(val),
            Err(e) => {
              println!("Failed to parse the line. Error: {}", e);
              return self.next();
            }
          }
        }
        None => return None,
      }
    }
  }
}
