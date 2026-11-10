using System;
using System.Windows;
using System.Windows.Media;
using SmartSQL.Helper;
using SmartSQL.Views;
using HandyControl.Data;
using System.Text.Json;
using System.Text.Json.Serialization;
using Newtonsoft.Json;
using System.Data;
using ClosedXML.Excel;
using SmartSQL.Framework.PhysicalDataModel;
using Newtonsoft.Json.Linq;
using CsvHelper;
using System.Globalization;
using System.IO;
using Microsoft.Win32;

namespace SmartSQL.UserControl
{
    /// <summary>
    /// MainContent.xaml 的交互逻辑
    /// </summary>
    public partial class UcJsonToExcel : BaseUserControl
    {
        public UcJsonToExcel()
        {
            InitializeComponent();
            DataContext = this;
            HighlightingProvider.Register(SkinType.Dark, new HighlightingProviderDark());
            TextEditor.TextArea.TextView.ElementGenerators.Add(new TruncateLongLines());
            TextEditor.SyntaxHighlighting = HighlightingProvider.GetDefinition(SkinType.Dark, "Json");
            TextEditor.TextArea.SelectionCornerRadius = 0;
            TextEditor.TextArea.SelectionBorder = null;
            TextEditor.TextArea.SelectionForeground = null;
            TextEditor.TextArea.SelectionBrush = new SolidColorBrush((Color)ColorConverter.ConvertFromString("#FFADD6FF"));
        }

        /// <summary>
        /// 格式化Json
        /// </summary>
        /// <param name="sender"></param>
        /// <param name="e"></param>
        private void BtnFormatter_Click(object sender, RoutedEventArgs e)
        {
            #region MyRegion
            try
            {
                if (string.IsNullOrEmpty(TextEditor.Text))
                {
                    Oops.Oh("请输入Json文本");
                    return;
                }
                TextEditor.Text = StrUtil.JsonFormatter(TextEditor.Text);
            }
            catch (Exception)
            {
                Oops.Oh("Json解析失败，请检查");
            }
            #endregion
        }

        /// <summary>
        /// 压缩Json
        /// </summary>
        /// <param name="sender"></param>
        /// <param name="e"></param>
        private void BtnCompress_Click(object sender, RoutedEventArgs e)
        {
            #region MyRegion
            try
            {
                if (string.IsNullOrEmpty(TextEditor.Text))
                {
                    Oops.Oh("请输入Json文本");
                    return;
                }
                TextEditor.Text = StrUtil.JsonCompress(TextEditor.Text);
            }
            catch (Exception)
            {
                Oops.Oh("Json解析失败，请检查");
            }
            #endregion
        }

        /// <summary>
        /// 返回
        /// </summary>
        /// <param name="sender"></param>
        /// <param name="e"></param>
        private void BtnReturn_Click(object sender, RoutedEventArgs e)
        {
            var parentWindow = (ToolBox)System.Windows.Window.GetWindow(this);
            parentWindow.UcBox.Content = new UcMainTools();
        }

        /// <summary>
        /// 复制文本
        /// </summary>
        /// <param name="sender"></param>
        /// <param name="e"></param>
        private void BtnCopy_Click(object sender, RoutedEventArgs e)
        {
            if (!string.IsNullOrEmpty(TextEditor.Text))
            {
                TextEditor.SelectAll();
                Clipboard.SetDataObject(TextEditor.Text);
                Oops.Success("文本已复制到剪切板");
            }
        }

        /// <summary>
        /// 清空输入框
        /// </summary>
        /// <param name="sender"></param>
        /// <param name="e"></param>
        private void BtnClear_Click(object sender, RoutedEventArgs e)
        {
            #region MyRegion
            TextEditor.Text = string.Empty;
            TableGrid.ItemsSource = null;
            NoDataTextExt.Visibility = Visibility.Visible;
            #endregion
        }

        /// <summary>
        /// 编辑器获取焦点自动粘贴剪切板文本
        /// </summary>
        /// <param name="sender"></param>
        /// <param name="e"></param>
        private void TextEditor_GotFocus(object sender, RoutedEventArgs e)
        {
            #region MyRegion
            var clipboardText = Clipboard.GetText();
            if (string.IsNullOrEmpty(TextEditor.Text) && !string.IsNullOrEmpty(clipboardText))
            {
                var isTryParse = false;
                try
                {
                    JsonDocument.Parse(clipboardText);
                    isTryParse = true;
                }
                catch { }
                if (isTryParse)
                {
                    TextEditor.Text = clipboardText;
                }
            }
            #endregion
        }

        /// <summary>
        /// 转换为Excel
        /// </summary>
        /// <param name="sender"></param>
        /// <param name="e"></param>
        private void BtnConvert_Click(object sender, RoutedEventArgs e)
        {
            #region MyRegion
            try
            {
                var json = TextEditor.Text;
                if (string.IsNullOrEmpty(json))
                {
                    Oops.Oh("请输入Json文本");
                    return;
                }
                JToken token = JToken.Parse(json);
                if (token.Type == JTokenType.Object)
                {
                    // 如果是单个 JSON 对象，将其转换为包含该对象的数组
                    var jsonArray = new JArray(token);
                    json = jsonArray.ToString();
                }
                var dataTable = JsonConvert.DeserializeObject<DataTable>(json);
                TableGrid.ItemsSource = dataTable.DefaultView;
                NoDataTextExt.Visibility = dataTable.Rows.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
            }
            catch (Exception)
            {
                Oops.Oh("Json格式有误，请检查");
            }
            #endregion
        }

        /// <summary>
        /// 另存为
        /// </summary>
        /// <param name="sender"></param>
        /// <param name="e"></param>
        private void BtnSave_Click(object sender, RoutedEventArgs e)
        {
            CMeneExport.IsOpen = true;
        }

        /// <summary>
        /// 保存为CSV
        /// </summary>
        /// <param name="sender"></param>
        /// <param name="e"></param>
        private void MenuSaveCsv_Click(object sender, RoutedEventArgs e)
        {
            #region MyRegion
            var json = TextEditor.Text;
            if (string.IsNullOrEmpty(json))
            {
                Oops.Oh("请输入Json文本");
                return;
            }
            JToken token = JToken.Parse(json);
            if (token.Type == JTokenType.Object)
            {
                // 如果是单个 JSON 对象，将其转换为包含该对象的数组
                var jsonArray = new JArray(token);
                json = jsonArray.ToString();
            }
            var dataTable = JsonConvert.DeserializeObject<DataTable>(json);
            var saveFileDialog = new SaveFileDialog();
            saveFileDialog.Filter = "CSV文件(*.csv)|*.csv";
            if (saveFileDialog.ShowDialog() == true)
            {
                using (var writer = new StreamWriter(saveFileDialog.FileName))
                using (var csv = new CsvWriter(writer, CultureInfo.InvariantCulture))
                {
                    foreach (DataColumn column in dataTable.Columns)
                    {
                        csv.WriteField(column.ColumnName);
                    }
                    csv.NextRecord();

                    foreach (DataRow row in dataTable.Rows)
                    {
                        for (var i = 0; i < dataTable.Columns.Count; i++)
                        {
                            csv.WriteField(row[i]);
                        }
                        csv.NextRecord();
                    }
                    Oops.Success("保存成功");
                }
            }
            #endregion
        }

        /// <summary>
        /// 保存为Excel
        /// </summary>
        /// <param name="sender"></param>
        /// <param name="e"></param>
        private void MenuSaveExcel_Click(object sender, RoutedEventArgs e)
        {
            #region MyRegion
            var json = TextEditor.Text;
            if (string.IsNullOrEmpty(json))
            {
                Oops.Oh("请输入Json文本");
                return;
            }
            // 创建Excel文件
            using (var workbook = new XLWorkbook())
            {
                var worksheet = workbook.Worksheets.Add("Sheet1");
                JToken token = JToken.Parse(json);
                if (token.Type == JTokenType.Object)
                {
                    // 如果是单个 JSON 对象，将其转换为包含该对象的数组
                    var jsonArray = new JArray(token);
                    json = jsonArray.ToString();
                }
                var dataTable = JsonConvert.DeserializeObject<DataTable>(json);
                worksheet.Cell(1, 1).InsertTable(dataTable);
                // 保存Excel文件
                var saveFileDialog = new Microsoft.Win32.SaveFileDialog
                {
                    Filter = "Excel文件|*.xlsx",
                    Title = "Save Excel File"
                };
                if (saveFileDialog.ShowDialog() == true)
                {
                    workbook.SaveAs(saveFileDialog.FileName);
                    Oops.Success("保存成功");
                }
            }
            #endregion
        }
    }
}
